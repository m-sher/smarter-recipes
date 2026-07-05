//! Fetch recipes from web pages (schema.org Recipe JSON-LD preferred).

use super::RecipeSourceIngest;
use crate::domain::{Recipe, RecipeMeta, RecipeSource};
use crate::normalize::normalize_line;
use anyhow::{bail, Context, Result};
use scraper::{Html, Selector};
use serde_json::Value;
use std::time::Duration;
use url::Url;

/// A JSON-LD `url`/`@id` is safe to adopt as recipe identity only when it is the
/// **same host** as the page we fetched and points at an actual page (not a
/// fragment-only `@id` like `https://site.com/#recipe`).
fn canonical_is_trustworthy(canonical: &str, fetch_url: &str) -> bool {
    let (Ok(c), Ok(f)) = (Url::parse(canonical), Url::parse(fetch_url)) else {
        return false;
    };
    c.host_str() == f.host_str() && !c.path().trim_end_matches('/').is_empty()
}

#[derive(Clone)]
pub struct UrlSource {
    pub timeout: Duration,
    /// When set, this HTML is parsed; no network fetch occurs.
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
            // Prefer schema.org canonical url/@id for identity, but only when it's
            // same-host and not a fragment-only site-root id; else the fetch URL.
            let identity = recipe
                .meta
                .source_url
                .clone()
                .filter(|canon| canonical_is_trustworthy(canon, url))
                .unwrap_or_else(|| url.to_string());
            recipe.source = RecipeSource::Url {
                url: identity.clone(),
            };
            recipe.meta.source_url = Some(identity);
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

fn fetch_html(url: &str, timeout: Duration) -> Result<String> {
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

/// Collect string values from a JSON-LD field, decoding HTML entities.
fn json_ld_string_list(v: &Value) -> Vec<String> {
    raw_json_ld_string_list(v)
        .into_iter()
        .map(|s| crate::text::sanitize(&s))
        .collect()
}

fn raw_json_ld_string_list(v: &Value) -> Vec<String> {
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

/// Parse the leading numeric value from a nutrition string, handling US
/// (`"1,234.5"`) and European (`"1.234,5"`, `"12,5"`) grouping/decimal marks.
/// A lone comma is a decimal point unless it groups exactly three trailing
/// digits (`"1,234"` → 1234, `"12,5"` → 12.5).
fn leading_number(s: &str) -> Option<f64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let run: String = s[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .collect();
    let run = run.trim_end_matches([',', '.']);
    if run.is_empty() {
        return None;
    }
    parse_grouped_number(run)
}

/// Interpret a digit/`,`/`.` run into an `f64`, resolving which mark is the
/// decimal separator.
fn parse_grouped_number(run: &str) -> Option<f64> {
    let has_dot = run.contains('.');
    let has_comma = run.contains(',');
    let normalized = match (has_dot, has_comma) {
        // Both present: the last-occurring mark is the decimal separator.
        (true, true) => {
            let dec = if run.rfind(',') > run.rfind('.') {
                ','
            } else {
                '.'
            };
            let group = if dec == ',' { '.' } else { ',' };
            run.replace(group, "").replace(dec, ".")
        }
        // Only commas: grouping if every group after the first is 3 digits.
        (false, true) => {
            let parts: Vec<&str> = run.split(',').collect();
            let grouping = parts.len() > 1 && parts[1..].iter().all(|p| p.len() == 3);
            if grouping {
                parts.concat()
            } else {
                format!("{}.{}", parts[0], parts[1..].concat())
            }
        }
        _ => run.to_string(),
    };
    normalized.parse().ok()
}

/// Numeric value of a nutrition field: bare number, string, or a schema.org
/// `QuantitativeValue`-style object with a `value` field.
fn nutrition_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => leading_number(s),
        Value::Object(o) => o.get("value").and_then(nutrition_number),
        _ => None,
    }
}

/// Energy in kcal. Convert when the value is explicitly labeled kJ and not kcal.
fn nutrition_energy_kcal(v: &Value) -> Option<f64> {
    let value = nutrition_number(v)?;
    let unit_text = match v {
        Value::String(s) => s.to_lowercase(),
        Value::Object(o) => o
            .get("unitText")
            .or_else(|| o.get("unitCode"))
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_lowercase(),
        _ => String::new(),
    };
    if unit_text.contains("kj") && !unit_text.contains("kcal") {
        Some(value / 4.184)
    } else {
        Some(value)
    }
}

/// Parse a schema.org `NutritionInformation` object (per serving).
fn nutrition_from_json_ld(v: &Value) -> Option<crate::domain::Nutrition> {
    let obj = v.as_object()?;
    let n = crate::domain::Nutrition {
        kcal: obj.get("calories").and_then(nutrition_energy_kcal),
        protein_g: obj.get("proteinContent").and_then(nutrition_number),
        fat_g: obj.get("fatContent").and_then(nutrition_number),
        carbs_g: obj.get("carbohydrateContent").and_then(nutrition_number),
    };
    (!n.is_empty()).then_some(n)
}

fn recipe_from_json_ld(obj: &Value) -> Option<Recipe> {
    let title = crate::text::sanitize(obj.get("name").and_then(|n| n.as_str())?);

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
        let list = json_ld_string_list(cats);
        if !list.is_empty() {
            meta.category = Some(list.join(", "));
        }
    }
    if let Some(n) = obj.get("nutrition") {
        meta.nutrition = nutrition_from_json_ld(n);
    }
    // Prefer schema.org Recipe url, then absolute http(s) @id.
    if let Some(u) = obj
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
    {
        meta.source_url = Some(u.to_string());
    } else if let Some(u) = obj
        .get("@id")
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
    {
        meta.source_url = Some(u.to_string());
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
        .map(|e| crate::text::sanitize(e.text().collect::<String>().trim()))
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
    fn json_ld_captures_recipe_category() {
        // A single recipeCategory string.
        let one = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Recipe","name":"Tahini Sauce",
         "recipeCategory":"Sauce","recipeIngredient":["1/2 cup tahini","2 tbsp lemon juice"]}
        </script></head><body></body></html>"#;
        let src = UrlSource {
            offline_html: Some(one.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/tahini").unwrap();
        assert_eq!(r.meta.category.as_deref(), Some("Sauce"));

        // An array of categories is joined.
        let many = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Recipe","name":"Chili",
         "recipeCategory":["Main Course","Dinner"],"recipeIngredient":["1 lb beef","1 can beans"]}
        </script></head><body></body></html>"#;
        let src = UrlSource {
            offline_html: Some(many.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/chili").unwrap();
        assert_eq!(r.meta.category.as_deref(), Some("Main Course, Dinner"));
    }

    #[test]
    fn json_ld_ingredients_decode_html_entities() {
        // Sites embed raw HTML entities in JSON-LD `recipeIngredient` values;
        // they must not leak into ingredient text.
        let html = r#"
        <html><head>
        <script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Recipe","name":"Entity Test",
         "recipeIngredient":["salt &amp; pepper","1 cup&nbsp;flour"]}
        </script>
        </head><body></body></html>
        "#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/e").unwrap();
        let originals: Vec<&str> = r.ingredients.iter().map(|l| l.original.as_str()).collect();
        assert!(
            originals
                .iter()
                .all(|o| !o.contains("&nbsp") && !o.contains("&amp")),
            "raw entities leaked: {originals:?}"
        );
        assert!(
            originals.iter().any(|o| o.contains("salt & pepper")),
            "{originals:?}"
        );
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

    #[test]
    fn json_ld_canonical_url_overrides_fetch_url() {
        let html = r#"
        <script type="application/ld+json">
        {
          "@type": "Recipe",
          "name": "Test Cake",
          "url": "https://example.com/test-cake",
          "recipeIngredient": ["1 cup flour"]
        }
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let recipe = src.ingest("https://example.com/category/dessert").unwrap();
        match &recipe.source {
            RecipeSource::Url { url } => {
                assert_eq!(url, "https://example.com/test-cake");
            }
            other => panic!("expected RecipeSource::Url, got {other:?}"),
        }
        assert_eq!(
            recipe.meta.source_url.as_deref(),
            Some("https://example.com/test-cake")
        );
    }

    #[test]
    fn json_ld_at_id_used_when_url_missing() {
        let html = r#"
        <script type="application/ld+json">
        {
          "@type": "Recipe",
          "name": "Test Bread",
          "@id": "https://example.com/test-bread",
          "recipeIngredient": ["2 cups flour"]
        }
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let recipe = src.ingest("https://example.com/category/bread").unwrap();
        match &recipe.source {
            RecipeSource::Url { url } => {
                assert_eq!(url, "https://example.com/test-bread");
            }
            other => panic!("expected RecipeSource::Url, got {other:?}"),
        }
        assert_eq!(
            recipe.meta.source_url.as_deref(),
            Some("https://example.com/test-bread")
        );
    }

    #[test]
    fn fetch_url_kept_when_json_ld_has_no_canonical() {
        let html = r#"
        <script type="application/ld+json">
        {
          "@type": "Recipe",
          "name": "Plain Soup",
          "recipeIngredient": ["1 onion"]
        }
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let fetch = "https://example.com/plain-soup";
        let recipe = src.ingest(fetch).unwrap();
        match &recipe.source {
            RecipeSource::Url { url } => assert_eq!(url, fetch),
            other => panic!("expected RecipeSource::Url, got {other:?}"),
        }
        assert_eq!(recipe.meta.source_url.as_deref(), Some(fetch));
    }

    #[test]
    fn foreign_host_canonical_rejected() {
        let html = r#"<script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Recipe","name":"X",
           "url":"https://OTHER-site.com/stolen","recipeIngredient":["1 cup flour"]}
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let fetch = "https://example.com/real-page";
        let r = src.ingest(fetch).unwrap();
        // Cross-host canonical must NOT be adopted as identity.
        assert_eq!(r.meta.source_url.as_deref(), Some(fetch));
    }

    #[test]
    fn fragment_only_at_id_rejected() {
        let html = r#"<script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Recipe","name":"Y",
           "@id":"https://example.com/#recipe","recipeIngredient":["1 cup flour"]}
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let fetch = "https://example.com/soup-page";
        let r = src.ingest(fetch).unwrap();
        assert_eq!(r.meta.source_url.as_deref(), Some(fetch));
    }

    #[test]
    fn json_ld_nutrition_parsed_per_serving() {
        let html = r#"<script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Recipe","name":"N",
           "recipeIngredient":["1 cup flour"],
           "nutrition":{"@type":"NutritionInformation","calories":"344 kcal",
                        "proteinContent":"12.5 g","fatContent":"9 g",
                        "carbohydrateContent":"52 g"}}
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/n").unwrap();
        let n = r.meta.nutrition.expect("nutrition captured");
        assert_eq!(n.kcal, Some(344.0));
        assert_eq!(n.protein_g, Some(12.5));
        assert_eq!(n.fat_g, Some(9.0));
        assert_eq!(n.carbs_g, Some(52.0));
    }

    #[test]
    fn json_ld_nutrition_absent_or_empty_is_none() {
        let html = r#"<script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Recipe","name":"M",
           "recipeIngredient":["1 egg"],
           "nutrition":{"@type":"NutritionInformation","calories":"unknown"}}
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/m").unwrap();
        assert!(r.meta.nutrition.is_none());
    }

    #[test]
    fn leading_number_us_and_eu_separators() {
        // US thousands grouping and decimals.
        assert_eq!(leading_number("1,234 kcal"), Some(1234.0));
        assert_eq!(leading_number("1,234.5 kcal"), Some(1234.5));
        assert_eq!(leading_number("12.5 g"), Some(12.5));
        // European decimal comma must NOT inflate.
        assert_eq!(leading_number("12,5 g"), Some(12.5));
        assert_eq!(leading_number("8,44g"), Some(8.44));
        assert_eq!(leading_number("0,5 g"), Some(0.5));
        assert_eq!(leading_number("1.234,5 kcal"), Some(1234.5));
        // Misc.
        assert_eq!(leading_number("about 20g"), Some(20.0));
        assert_eq!(leading_number("n/a"), None);
    }

    #[test]
    fn kilojoule_energy_converted_to_kcal() {
        let html = r#"<script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Recipe","name":"KJ",
           "recipeIngredient":["1 cup flour"],
           "nutrition":{"@type":"NutritionInformation","calories":"1441 kJ"}}
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let r = src.ingest("https://example.com/kj").unwrap();
        let kcal = r.meta.nutrition.unwrap().kcal.unwrap();
        assert!((kcal - 344.4).abs() < 1.0, "kcal = {kcal}");
    }

    #[test]
    fn quantitative_value_object_and_decimal_comma_protein() {
        let html = r#"<script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Recipe","name":"QV",
           "recipeIngredient":["1 cup flour"],
           "nutrition":{"@type":"NutritionInformation",
             "calories":{"@type":"QuantitativeValue","value":"344","unitText":"kcal"},
             "proteinContent":"12,5 g"}}
        </script>"#;
        let src = UrlSource {
            offline_html: Some(html.into()),
            ..Default::default()
        };
        let n = src
            .ingest("https://example.com/qv")
            .unwrap()
            .meta
            .nutrition
            .unwrap();
        assert_eq!(n.kcal, Some(344.0));
        assert_eq!(n.protein_g, Some(12.5));
    }
}
