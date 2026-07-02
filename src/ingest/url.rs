//! Fetch recipes from web pages (schema.org Recipe JSON-LD preferred).

use super::RecipeSourceIngest;
use crate::domain::{Recipe, RecipeMeta, RecipeSource};
use crate::normalize::normalize_line;
use anyhow::{bail, Context, Result};
use scraper::{Html, Selector};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct UrlSource {
    pub timeout: Duration,
    /// When set, use this HTML instead of fetching (for offline tests).
    pub offline_html: Option<String>,
}

impl Default for UrlSource {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            offline_html: None,
        }
    }
}

impl RecipeSourceIngest for UrlSource {
    fn name(&self) -> &'static str {
        "url"
    }

    fn ingest(&self, input: &str) -> Result<Recipe> {
        let url = input.trim();
        let html = if let Some(ref h) = self.offline_html {
            h.clone()
        } else {
            fetch_html(url, self.timeout)?
        };

        if let Some(mut recipe) = extract_json_ld_recipe(&html)? {
            recipe.source = RecipeSource::Url {
                url: url.to_string(),
            };
            if recipe.meta.source_url.is_none() {
                recipe.meta.source_url = Some(url.to_string());
            }
            return Ok(recipe);
        }

        let mut recipe = extract_heuristic(&html)?;
        recipe.source = RecipeSource::Url {
            url: url.to_string(),
        };
        recipe.meta.source_url = Some(url.to_string());
        Ok(recipe)
    }
}

pub(crate) fn fetch_html(url: &str, timeout: Duration) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(concat!(
            "smarter-recipes/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/local/smarter-recipes)"
        ))
        .build()?;
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!("HTTP {} fetching {url}", resp.status());
    }
    resp.text().context("reading response body")
}

/// Walk JSON values looking for @type Recipe (string or array).
fn is_recipe_type(v: &Value) -> bool {
    match v.get("@type") {
        Some(Value::String(s)) => s.eq_ignore_ascii_case("Recipe"),
        Some(Value::Array(arr)) => arr
            .iter()
            .any(|t| t.as_str().is_some_and(|s| s.eq_ignore_ascii_case("Recipe"))),
        _ => false,
    }
}

fn find_recipe_objects(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(map) => {
            if is_recipe_type(v) {
                out.push(v.clone());
            }
            // Recurse into all values once (covers @graph and every other field).
            for val in map.values() {
                find_recipe_objects(val, out);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                find_recipe_objects(item, out);
            }
        }
        _ => {}
    }
}

fn json_ld_string_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o
                    .get("text")
                    .or_else(|| o.get("name"))
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            })
            .collect(),
        Value::Object(o) => o
            .get("text")
            .or_else(|| o.get("name"))
            .and_then(|t| t.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
        _ => vec![],
    }
}

fn parse_servings(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            // "4 servings" → 4
            let num: String = s
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            num.parse().ok()
        }
        _ => None,
    }
}

fn recipe_from_json_ld(obj: &Value) -> Option<Recipe> {
    let title = obj.get("name").and_then(|n| n.as_str())?.to_string();

    let mut recipe = Recipe::new(title);
    recipe.servings = obj.get("recipeYield").and_then(parse_servings);

    if let Some(ings) = obj.get("recipeIngredient") {
        recipe.ingredients = json_ld_string_list(ings)
            .into_iter()
            .map(|s| normalize_line(&s))
            .collect();
    }

    if let Some(inst) = obj.get("recipeInstructions") {
        recipe.steps = flatten_instructions(inst);
    }

    let mut meta = RecipeMeta::default();
    if let Some(a) = obj.get("author") {
        meta.author = author_name(a);
    }
    if let Some(c) = obj.get("recipeCuisine").and_then(|c| c.as_str()) {
        meta.cuisine = Some(c.to_string());
    }
    if let Some(cats) = obj.get("recipeCategory") {
        meta.tags = json_ld_string_list(cats);
    }
    recipe.meta = meta;
    Some(recipe)
}

fn author_name(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string()),
        Value::Array(arr) => arr.first().and_then(author_name),
        _ => None,
    }
}

fn flatten_instructions(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => s
            .split('\n')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|s| s.to_string())
            .collect(),
        Value::Array(arr) => arr.iter().flat_map(flatten_instructions).collect(),
        Value::Object(o) => {
            // HowToStep / HowToSection
            if let Some(t) = o.get("text").and_then(|t| t.as_str()) {
                return vec![t.to_string()];
            }
            if let Some(items) = o.get("itemListElement") {
                return flatten_instructions(items);
            }
            if let Some(n) = o.get("name").and_then(|n| n.as_str()) {
                return vec![n.to_string()];
            }
            vec![]
        }
        _ => vec![],
    }
}

fn extract_json_ld_recipe(html: &str) -> Result<Option<Recipe>> {
    let document = Html::parse_document(html);
    let selector = Selector::parse(r#"script[type="application/ld+json"]"#).unwrap();
    let mut recipes = Vec::new();
    for el in document.select(&selector) {
        let text = el.text().collect::<String>();
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        // Some pages embed multiple JSON objects; try parse as Value
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            find_recipe_objects(&v, &mut recipes);
        }
    }
    Ok(recipes.first().and_then(recipe_from_json_ld))
}

/// Best-effort HTML heuristics when JSON-LD is missing.
fn extract_heuristic(html: &str) -> Result<Recipe> {
    let document = Html::parse_document(html);
    let title_sel = Selector::parse("h1").unwrap();
    let title = document
        .select(&title_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Untitled recipe".into());

    let mut ingredients = Vec::new();
    // Common class/id patterns
    for sel in [
        "[itemprop=recipeIngredient]",
        ".recipe-ingredient",
        ".ingredients li",
        "ul.ingredients li",
        ".wprm-recipe-ingredient",
    ] {
        if let Ok(selector) = Selector::parse(sel) {
            for el in document.select(&selector) {
                let t = el.text().collect::<String>();
                let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                if !t.is_empty() {
                    ingredients.push(normalize_line(&t));
                }
            }
            if !ingredients.is_empty() {
                break;
            }
        }
    }

    let mut steps = Vec::new();
    for sel in [
        "[itemprop=recipeInstructions] li",
        ".recipe-instruction",
        ".instructions li",
        ".wprm-recipe-instruction-text",
    ] {
        if let Ok(selector) = Selector::parse(sel) {
            for el in document.select(&selector) {
                let t = el.text().collect::<String>();
                let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                if !t.is_empty() {
                    steps.push(t);
                }
            }
            if !steps.is_empty() {
                break;
            }
        }
    }

    if ingredients.is_empty() {
        bail!("could not extract ingredients from HTML (no JSON-LD Recipe and heuristics failed)");
    }

    let mut recipe = Recipe::new(title);
    recipe.ingredients = ingredients;
    recipe.steps = steps;
    Ok(recipe)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_HTML: &str = r#"
    <html><head>
    <script type="application/ld+json">
    {
      "@context": "https://schema.org",
      "@type": "Recipe",
      "name": "Chocolate Chip Cookies",
      "recipeYield": "24",
      "recipeIngredient": [
        "2 cups flour",
        "1 cup sugar",
        "1/2 cup butter"
      ],
      "recipeInstructions": [
        {"@type": "HowToStep", "text": "Mix ingredients."},
        {"@type": "HowToStep", "text": "Bake at 350F."}
      ],
      "author": {"@type": "Person", "name": "Ada"}
    }
    </script>
    </head><body><h1>Ignore me</h1></body></html>
    "#;

    #[test]
    fn parse_json_ld() {
        let src = UrlSource {
            offline_html: Some(SAMPLE_HTML.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/cookies").unwrap();
        assert_eq!(r.title, "Chocolate Chip Cookies");
        assert_eq!(r.servings, Some(24.0));
        assert_eq!(r.ingredients.len(), 3);
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.meta.author.as_deref(), Some("Ada"));
    }

    #[test]
    fn graph_embedded_recipe() {
        let html = r#"
        <script type="application/ld+json">
        {"@graph":[
          {"@type":"WebPage","name":"Page"},
          {"@type":"Recipe","name":"Soup","recipeIngredient":["1 onion"],"recipeInstructions":"Boil."}
        ]}
        </script>
        "#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/soup").unwrap();
        assert_eq!(r.title, "Soup");
        assert_eq!(r.ingredients.len(), 1);
    }
}
