//! Pluggable product / package sources (public API or HTML fixtures).
//!
//! [`OpenFoodFactsSource`] queries the Open Food Facts public API when online.
//! [`FixtureStoreSource`] loads recorded JSON for offline/CI tests.

use super::Package;
use crate::domain::UnitKind;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

/// Trait for fetching package sizes/prices for an ingredient query string.
pub trait ProductSource {
    fn name(&self) -> &str;
    fn fetch_packages(&self, query: &str) -> Result<Vec<Package>>;
}

/// Open Food Facts public search API (best-effort; prices often missing).
pub struct OpenFoodFactsSource {
    pub timeout: Duration,
    /// Override base URL (tests).
    pub base_url: String,
    /// Optional pre-canned response body (offline tests).
    pub offline_body: Option<String>,
}

impl Default for OpenFoodFactsSource {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            base_url: "https://world.openfoodfacts.org".into(),
            offline_body: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct OffSearch {
    products: Option<Vec<OffProduct>>,
}

#[derive(Debug, Deserialize)]
struct OffProduct {
    product_name: Option<String>,
    product_name_en: Option<String>,
    quantity: Option<String>,
    product_quantity: Option<f64>,
    product_quantity_unit: Option<String>,
}

impl ProductSource for OpenFoodFactsSource {
    fn name(&self) -> &str {
        "openfoodfacts"
    }

    fn fetch_packages(&self, query: &str) -> Result<Vec<Package>> {
        let body = if let Some(ref b) = self.offline_body {
            b.clone()
        } else {
            let url = format!(
                "{}/cgi/search.pl?search_terms={}&search_simple=1&action=process&json=1&page_size=10",
                self.base_url.trim_end_matches('/'),
                crate::net::encode_query(query)
            );
            let client = reqwest::blocking::Client::builder()
                .timeout(self.timeout)
                .user_agent(concat!(
                    "smarter-recipes/",
                    env!("CARGO_PKG_VERSION"),
                    " (meal planner; +https://github.com/m-sher/smarter-recipes)"
                ))
                .build()?;
            let resp = client.get(&url).send().context("Open Food Facts request")?;
            if !resp.status().is_success() {
                bail!("Open Food Facts HTTP {}", resp.status());
            }
            resp.text()?
        };

        let parsed: OffSearch = serde_json::from_str(&body).context("parsing OFF JSON")?;
        let mut out = Vec::new();
        for p in parsed.products.unwrap_or_default() {
            let label = p
                .product_name
                .or(p.product_name_en)
                .unwrap_or_else(|| query.to_string());
            if let Some(pkg) = off_to_package(
                &label,
                p.product_quantity,
                p.product_quantity_unit.as_deref(),
                p.quantity.as_deref(),
            ) {
                out.push(pkg);
            }
        }
        if out.is_empty() {
            bail!("no parseable products for '{query}' from {}", self.name());
        }
        Ok(out)
    }
}

fn off_to_package(
    label: &str,
    qty: Option<f64>,
    unit: Option<&str>,
    quantity_text: Option<&str>,
) -> Option<Package> {
    let (size, kind) = if let (Some(q), Some(u)) = (qty, unit) {
        parse_off_unit(q, u)?
    } else if let Some(text) = quantity_text {
        parse_quantity_text(text)?
    } else {
        return None;
    };
    let qty_label = quantity_text.unwrap_or("pack");
    Some(Package {
        label: format!("{label} ({qty_label})"),
        size_canonical: size,
        price_cents: None,
        kind,
    })
}

fn parse_off_unit(q: f64, u: &str) -> Option<(f64, UnitKind)> {
    let u = u.to_lowercase();
    match u.as_str() {
        "g" | "gr" | "gram" | "grams" => Some((q, UnitKind::Mass)),
        "kg" => Some((q * 1000.0, UnitKind::Mass)),
        "ml" | "milliliter" | "millilitre" => Some((q, UnitKind::Volume)),
        "l" | "liter" | "litre" => Some((q * 1000.0, UnitKind::Volume)),
        "cl" => Some((q * 10.0, UnitKind::Volume)),
        "oz" => Some((q * 28.349523125, UnitKind::Mass)),
        "fl oz" => Some((q * 29.5735295625, UnitKind::Volume)),
        _ => None,
    }
}

fn parse_quantity_text(text: &str) -> Option<(f64, UnitKind)> {
    let t = text.trim().to_lowercase();
    // e.g. "500 g", "1 l", "16 oz"
    let mut parts = t.split_whitespace();
    let num: f64 = parts.next()?.replace(',', ".").parse().ok()?;
    let unit = parts.next().unwrap_or("g");
    parse_off_unit(num, unit)
}

/// Load packages from a recorded fixture file (JSON array of Package, or map of query → packages).
pub struct FixtureStoreSource {
    pub path: std::path::PathBuf,
}

impl FixtureStoreSource {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl ProductSource for FixtureStoreSource {
    fn name(&self) -> &str {
        "fixture"
    }

    fn fetch_packages(&self, query: &str) -> Result<Vec<Package>> {
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading fixture {}", self.path.display()))?;
        // Try map first
        if let Ok(map) =
            serde_json::from_str::<std::collections::HashMap<String, Vec<Package>>>(&text)
        {
            let q = query.to_lowercase();
            if let Some(p) = map.get(&q) {
                return Ok(p.clone());
            }
            // substring key match
            for (k, v) in &map {
                if q.contains(k) || k.contains(&q) {
                    return Ok(v.clone());
                }
            }
            bail!("fixture has no entry for '{query}'");
        }
        let list: Vec<Package> = serde_json::from_str(&text)?;
        Ok(list)
    }
}

/// Fetch packages for each ingredient name, merging into the catalog; on failure keep defaults.
pub fn enrich_catalog_from_source(
    catalog: &mut super::PackageCatalog,
    source: &dyn ProductSource,
    ingredient_names: &[String],
) -> Vec<String> {
    let mut notes = Vec::new();
    for name in ingredient_names {
        match source.fetch_packages(name) {
            Ok(pkgs) if !pkgs.is_empty() => {
                catalog.merge_packages(name, pkgs);
                notes.push(format!("enriched '{name}' from {}", source.name()));
            }
            Ok(_) => notes.push(format!("no products for '{name}' from {}", source.name())),
            Err(e) => notes.push(format!(
                "fallback catalog for '{name}' ({}: {e})",
                source.name()
            )),
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn fixture_source_loads() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"{{"milk": [{{"label": "1L milk", "size_canonical": 1000, "price_cents": 150, "kind": "volume"}}]}}"#
        )
        .unwrap();
        let src = FixtureStoreSource::new(f.path());
        let pkgs = src.fetch_packages("milk").unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].price_cents, Some(150));
    }

    #[test]
    fn openfoodfacts_offline_body() {
        let body = r#"{
          "products": [
            {
              "product_name": "Organic Milk",
              "product_quantity": 1000,
              "product_quantity_unit": "ml",
              "quantity": "1 l"
            }
          ]
        }"#;
        let src = OpenFoodFactsSource {
            offline_body: Some(body.into()),
            ..Default::default()
        };
        let pkgs = src.fetch_packages("milk").unwrap();
        assert_eq!(pkgs.len(), 1);
        assert!((pkgs[0].size_canonical - 1000.0).abs() < 0.1);
        assert_eq!(pkgs[0].kind, UnitKind::Volume);
    }
}
