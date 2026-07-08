//! Thin blocking Gemini `generateContent` client for category labeling.

use super::prompt::{system_instruction, user_batch_message};
use super::vocab::filter_labels;
use super::{CategoryLabeler, LabelResult, RecipeSnippet};
use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
const MAX_RETRIES: u32 = 5;

/// Gemini Flash labeler via REST.
pub struct GeminiLabeler {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl GeminiLabeler {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .user_agent("smarter-recipes/categorize")
            .build()
            .context("building HTTP client for Gemini")?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            model: model.into(),
            base_url: DEFAULT_BASE.into(),
        })
    }

    #[cfg(test)]
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = base.into();
        self
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/models/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            self.model
        )
    }

    fn request_body(items: &[RecipeSnippet]) -> Value {
        json!({
            "system_instruction": {
                "parts": [{ "text": system_instruction() }]
            },
            "contents": [{
                "role": "user",
                "parts": [{ "text": user_batch_message(items) }]
            }],
            "generationConfig": {
                "temperature": 0.0,
                "responseMimeType": "application/json"
            }
        })
    }

    fn post_once(&self, body: &Value) -> Result<String> {
        let resp = self
            .client
            .post(self.endpoint())
            .header("x-goog-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .context("Gemini request failed")?;
        let status = resp.status();
        let text = resp.text().context("reading Gemini response body")?;
        if status.as_u16() == 429 || status.is_server_error() {
            bail!("retryable HTTP {status}: {}", truncate(&text, 200));
        }
        if !status.is_success() {
            bail!("Gemini HTTP {status}: {}", truncate(&text, 400));
        }
        Ok(text)
    }

    fn post_with_retry(&self, body: &Value) -> Result<String> {
        let mut last_err = None;
        for attempt in 0..MAX_RETRIES {
            match self.post_once(body) {
                Ok(t) => return Ok(t),
                Err(e) => {
                    let msg = format!("{e:#}");
                    let retryable = msg.contains("retryable HTTP");
                    last_err = Some(e);
                    if !retryable || attempt + 1 == MAX_RETRIES {
                        break;
                    }
                    let wait = Duration::from_millis(500 * 2u64.pow(attempt));
                    std::thread::sleep(wait);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Gemini request failed")))
    }
}

impl CategoryLabeler for GeminiLabeler {
    fn label_batch(&self, items: &[RecipeSnippet]) -> Result<Vec<LabelResult>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let body = Self::request_body(items);
        let raw = self.post_with_retry(&body)?;
        let text = extract_text_from_generate_content(&raw)
            .with_context(|| format!("parsing Gemini response: {}", truncate(&raw, 300)))?;
        parse_label_array(&text, items)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

/// Pull the model text from a generateContent JSON body.
pub fn extract_text_from_generate_content(body: &str) -> Result<String> {
    let v: Value = serde_json::from_str(body).context("Gemini response is not JSON")?;
    let text = v
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing candidates[0].content.parts[0].text"))?;
    Ok(strip_markdown_fence(text).to_string())
}

fn strip_markdown_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        return rest.trim().trim_end_matches("```").trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        return rest.trim().trim_end_matches("```").trim();
    }
    s
}

/// Parse model JSON array into one [`LabelResult`] per input id (missing → empty).
pub fn parse_label_array(text: &str, items: &[RecipeSnippet]) -> Result<Vec<LabelResult>> {
    let v: Value = serde_json::from_str(text.trim()).context("model output is not JSON")?;
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("model output is not a JSON array"))?;

    let mut by_id: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for el in arr {
        let id = el
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let cats: Vec<String> = el
            .get("categories")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        by_id.insert(id, filter_labels(&cats));
    }

    Ok(items
        .iter()
        .map(|item| LabelResult {
            id: item.id.clone(),
            categories: by_id.remove(&item.id).unwrap_or_default(),
        })
        .collect())
}

/// Resolve API key from explicit value or environment.
pub fn resolve_api_key(explicit: Option<&str>) -> Result<String> {
    if let Some(k) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(k.to_string());
    }
    if let Ok(k) = std::env::var("SMARTER_RECIPES_GEMINI_API_KEY") {
        if !k.trim().is_empty() {
            return Ok(k);
        }
    }
    if let Ok(k) = std::env::var("GEMINI_API_KEY") {
        if !k.trim().is_empty() {
            return Ok(k);
        }
    }
    bail!(
        "Gemini API key not set. Export SMARTER_RECIPES_GEMINI_API_KEY or GEMINI_API_KEY \
         (or pass --api-key). Get a key at https://aistudio.google.com/apikey"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_from_fixture() {
        let body = r#"{
          "candidates": [{
            "content": {
              "parts": [{"text": "[{\"id\":\"a\",\"categories\":[\"Beverage\"]}]"}]
            }
          }]
        }"#;
        let t = extract_text_from_generate_content(body).unwrap();
        assert!(t.contains("Beverage"));
    }

    #[test]
    fn parse_labels_filters_and_aligns() {
        let items = vec![
            RecipeSnippet {
                id: "a".into(),
                title: "Cooler".into(),
                ingredients: vec![],
                source_kind: "epub",
            },
            RecipeSnippet {
                id: "b".into(),
                title: "Mystery".into(),
                ingredients: vec![],
                source_kind: "epub",
            },
        ];
        let text = r#"[
            {"id":"a","categories":["beverage","not-a-label"]},
            {"id":"b","categories":[]}
        ]"#;
        let out = parse_label_array(text, &items).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].categories, vec!["Beverage".to_string()]);
        assert!(out[1].categories.is_empty());
    }

    #[test]
    fn strip_fence() {
        let s = "```json\n[{\"id\":\"x\",\"categories\":[]}]\n```";
        assert!(strip_markdown_fence(s).starts_with('['));
    }
}
