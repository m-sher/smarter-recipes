//! Open Food Facts nutrition source — a keyless fallback used when FoodData
//! Central is rate-limited. Uses the Search-a-licious full-text API
//! (`search.openfoodfacts.org`) and reads per-100 g macros from the
//! best-matching product's `nutriments`.
//!
//! The legacy `cgi/search.pl` endpoint is frequently overloaded (HTTP 503), so
//! this deliberately uses the newer search service. No API key is required and
//! quotas are independent of FDC, keeping `nutrition fetch` making progress
//! after the FDC DEMO_KEY limit is hit.

use super::{NutritionSource, RateGate, RateLimited};
use crate::domain::Macros;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::time::Duration;

const OFF_SEARCH_URL: &str = "https://search.openfoodfacts.org";
/// Politeness pause before each search request.
const OFF_REQUEST_DELAY: Duration = Duration::from_millis(300);
/// Retries for a transient failure (5xx / network error) before giving up.
const OFF_MAX_RETRIES: u32 = 2;

/// Open Food Facts Search-a-licious API as a [`NutritionSource`].
pub struct OpenFoodFactsNutritionSource {
    client: reqwest::blocking::Client,
    pub base_url: String,
    /// Shared dispatch gate so concurrent workers don't exceed the request rate.
    gate: RateGate,
    /// Canned response body for offline tests.
    pub offline_body: Option<String>,
}

impl Default for OpenFoodFactsNutritionSource {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!(
                "smarter-recipes/",
                env!("CARGO_PKG_VERSION"),
                " (meal planner; +https://github.com/m-sher/smarter-recipes)"
            ))
            .build()
            .expect("build blocking HTTP client");
        Self {
            client,
            base_url: OFF_SEARCH_URL.into(),
            gate: RateGate::new(OFF_REQUEST_DELAY),
            offline_body: None,
        }
    }
}

/// Search-a-licious wraps results in `hits`.
#[derive(Debug, Deserialize)]
struct OffSearch {
    hits: Option<Vec<OffHit>>,
}

#[derive(Debug, Deserialize)]
struct OffHit {
    nutriments: Option<serde_json::Map<String, serde_json::Value>>,
}

fn retry_backoff(attempt: u32) -> Duration {
    Duration::from_millis(400 * (1u64 << attempt.min(3)))
}

impl OpenFoodFactsNutritionSource {
    fn fetch_body(&self, query: &str) -> Result<String> {
        let url = format!(
            "{}/search?q={}&page_size=20",
            self.base_url.trim_end_matches('/'),
            crate::net::encode_query(query)
        );
        self.gate.wait();
        let mut attempt = 0u32;
        loop {
            let resp = match self.client.get(&url).send() {
                Ok(r) => r,
                Err(e) => {
                    if attempt < OFF_MAX_RETRIES {
                        attempt += 1;
                        std::thread::sleep(retry_backoff(attempt));
                        continue;
                    }
                    return Err(e).context("Open Food Facts request");
                }
            };
            let status = resp.status();
            if status.as_u16() == 429 {
                return Err(anyhow::Error::new(RateLimited {
                    using_demo_key: false,
                }));
            }
            // 5xx is transient (the search service is occasionally overloaded).
            if status.is_server_error() && attempt < OFF_MAX_RETRIES {
                attempt += 1;
                std::thread::sleep(retry_backoff(attempt));
                continue;
            }
            if !status.is_success() {
                bail!("Open Food Facts HTTP {status}");
            }
            return resp.text().context("reading Open Food Facts body");
        }
    }
}

/// Read a numeric OFF nutriment that may be encoded as a number or a string.
fn nutriment(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
    match map.get(key)? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|v| v.is_finite())
}

/// Per-100 g macros from one product's `nutriments`, or `None` without a usable
/// energy value. Energy prefers `energy-kcal_100g`; falls back to `energy_100g`
/// (kJ) converted to kcal.
fn macros_from_nutriments(n: &serde_json::Map<String, serde_json::Value>) -> Option<Macros> {
    let kcal = nutriment(n, "energy-kcal_100g")
        .filter(|&k| k > 0.0)
        .or_else(|| {
            nutriment(n, "energy_100g")
                .filter(|&kj| kj > 0.0)
                .map(|kj| kj / 4.184)
        })?;
    Some(Macros {
        kcal,
        protein_g: nutriment(n, "proteins_100g").unwrap_or(0.0),
        fat_g: nutriment(n, "fat_100g").unwrap_or(0.0),
        carbs_g: nutriment(n, "carbohydrates_100g").unwrap_or(0.0),
    })
}

impl NutritionSource for OpenFoodFactsNutritionSource {
    fn name(&self) -> &'static str {
        "openfoodfacts"
    }

    fn lookup(&self, ingredient: &str) -> Result<Option<Macros>> {
        let body = if let Some(ref b) = self.offline_body {
            b.clone()
        } else {
            self.fetch_body(ingredient)?
        };
        let parsed: OffSearch =
            serde_json::from_str(&body).context("parsing Open Food Facts response")?;
        // First hit carrying a usable energy value wins (results are
        // relevance-ranked, so the leading complete entry is representative).
        for hit in parsed.hits.unwrap_or_default() {
            if let Some(n) = &hit.nutriments {
                if let Some(m) = macros_from_nutriments(n) {
                    return Ok(Some(m));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn off(body: &str) -> Result<Option<Macros>> {
        OpenFoodFactsNutritionSource {
            offline_body: Some(body.to_string()),
            ..Default::default()
        }
        .lookup("banana")
    }

    #[test]
    fn parses_first_hit_with_kcal() {
        let body = r#"{"hits":[
            {"product_name":"No nutrition","nutriments":{}},
            {"product_name":"Banana","nutriments":{"energy-kcal_100g":89,"proteins_100g":1.1,"fat_100g":0.3,"carbohydrates_100g":23}}
        ]}"#;
        let m = off(body).unwrap().unwrap();
        assert!((m.kcal - 89.0).abs() < 1e-9);
        assert!((m.protein_g - 1.1).abs() < 1e-9);
        assert!((m.carbs_g - 23.0).abs() < 1e-9);
    }

    #[test]
    fn kj_energy_is_converted_to_kcal() {
        // Only kJ present: 380 kJ ≈ 90.8 kcal; string value tolerated.
        let body = r#"{"hits":[{"nutriments":{"energy_100g":380,"proteins_100g":"2"}}]}"#;
        let m = off(body).unwrap().unwrap();
        assert!((m.kcal - 380.0 / 4.184).abs() < 1e-6);
        assert!((m.protein_g - 2.0).abs() < 1e-9);
    }

    #[test]
    fn no_hits_or_no_energy_is_miss() {
        assert!(off(r#"{"hits":[]}"#).unwrap().is_none());
        assert!(off(r#"{"hits":[{"nutriments":{"proteins_100g":5}}]}"#)
            .unwrap()
            .is_none());
        assert!(off(r#"{}"#).unwrap().is_none());
    }
}
