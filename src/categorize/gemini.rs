//! Thin blocking Gemini `generateContent` client for category labeling.

use super::prompt::{system_instruction, user_batch_message};
use super::vocab::filter_labels;
use super::{CategoryLabeler, LabelResult, RecipeSnippet};
use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::fmt;
use std::time::Duration;

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
const MAX_RETRIES: u32 = 5;
const DEFAULT_RETRY_BASE: Duration = Duration::from_millis(500);

/// Error from a Gemini HTTP attempt, with an explicit retryability flag
/// (not string-matched).
#[derive(Debug)]
pub struct GeminiError {
    pub message: String,
    pub retryable: bool,
}

impl GeminiError {
    fn retryable(status: reqwest::StatusCode, body: &str) -> Self {
        Self {
            message: format!("HTTP {status}: {}", truncate(body, 200)),
            retryable: true,
        }
    }

    fn fatal(status: reqwest::StatusCode, body: &str) -> Self {
        Self {
            message: format!("HTTP {status}: {}", truncate(body, 400)),
            retryable: false,
        }
    }

    fn other(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            retryable: false,
        }
    }
}

impl fmt::Display for GeminiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.retryable {
            write!(f, "retryable Gemini error: {}", self.message)
        } else {
            write!(f, "Gemini error: {}", self.message)
        }
    }
}

impl std::error::Error for GeminiError {}

/// Gemini Flash labeler via REST.
pub struct GeminiLabeler {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
    /// Base delay for exponential backoff (shortened in tests).
    retry_base: Duration,
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
            retry_base: DEFAULT_RETRY_BASE,
        })
    }

    /// Point at a mock base URL (tests). Path `/models/{model}:generateContent` is appended.
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = base.into();
        self
    }

    /// Override retry backoff base (tests use a short value).
    pub fn with_retry_base(mut self, d: Duration) -> Self {
        self.retry_base = d;
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

    fn post_once(&self, body: &Value) -> Result<String, GeminiError> {
        let resp = self
            .client
            .post(self.endpoint())
            .header("x-goog-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .map_err(|e| GeminiError::other(format!("request failed: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| GeminiError::other(format!("reading body: {e}")))?;
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(GeminiError::retryable(status, &text));
        }
        if !status.is_success() {
            return Err(GeminiError::fatal(status, &text));
        }
        Ok(text)
    }

    fn post_with_retry(&self, body: &Value) -> Result<String> {
        let mut last_err: Option<GeminiError> = None;
        for attempt in 0..MAX_RETRIES {
            match self.post_once(body) {
                Ok(t) => return Ok(t),
                Err(e) => {
                    let retryable = e.retryable;
                    last_err = Some(e);
                    if !retryable || attempt + 1 == MAX_RETRIES {
                        break;
                    }
                    let wait = self.retry_base.saturating_mul(2u32.pow(attempt));
                    std::thread::sleep(wait);
                }
            }
        }
        match last_err {
            Some(e) => Err(e).context("Gemini request failed after retries"),
            None => bail!("Gemini request failed"),
        }
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

/// Truncate to at most `max` **bytes** on a char boundary (never panics on UTF-8).
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = s.floor_char_boundary(max);
    format!("{}…", &s[..end])
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

/// Resolve API key from explicit CLI value or environment.
///
/// Order: `explicit` → `SMARTER_RECIPES_GEMINI_API_KEY` → `GEMINI_API_KEY`.
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn truncate_is_utf8_safe_on_multibyte() {
        // 'é' is 2 bytes; max=200 used to panic with &s[..200] mid-char.
        let s = "café".repeat(60); // 240 bytes of non-ASCII-friendly content
        assert!(s.len() > 200);
        let t = truncate(&s, 200);
        assert!(t.ends_with('…'));
        assert!(t.is_char_boundary(t.len() - '…'.len_utf8()));
        // No panic; result is valid UTF-8 shorter than original+ellipsis
        let _ = t;
    }

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate("hi", 10), "hi");
    }

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

    #[test]
    fn gemini_error_retryable_flag() {
        let e = GeminiError::retryable(reqwest::StatusCode::TOO_MANY_REQUESTS, "slow down");
        assert!(e.retryable);
        let e2 = GeminiError::fatal(reqwest::StatusCode::BAD_REQUEST, "nope");
        assert!(!e2.retryable);
    }

    /// Local mock: 429 twice, then a valid generateContent body.
    #[test]
    fn post_with_retry_retries_429_then_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_t = hits.clone();
        thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let n = hits_t.fetch_add(1, Ordering::SeqCst);
                let (status, body) = if n < 2 {
                    ("429 Too Many Requests", "rate limited")
                } else {
                    (
                        "200 OK",
                        r#"{"candidates":[{"content":{"parts":[{"text":"[]"}]}}]}"#,
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let labeler = GeminiLabeler::new("test-key", "test-model")
            .unwrap()
            .with_base_url(format!("http://{addr}/v1beta"))
            .with_retry_base(Duration::from_millis(1));

        let items = vec![RecipeSnippet {
            id: "x".into(),
            title: "T".into(),
            ingredients: vec![],
            source_kind: "epub",
        }];
        let out = labeler.label_batch(&items).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].categories.is_empty());
        assert!(
            hits.load(Ordering::SeqCst) >= 3,
            "expected retries, hits={}",
            hits.load(Ordering::SeqCst)
        );
    }
}
