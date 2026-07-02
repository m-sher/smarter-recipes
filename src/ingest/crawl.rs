//! Crawl an index/parent page for recipe links and ingest new ones.
//!
//! Discovery follows the pattern "a parent page like `site.com/recipes` links to
//! child recipe pages like `site.com/recipes/some-dish`": links are kept when they
//! are same-host descendants of the parent path. Each candidate is parsed with
//! [`UrlSource`]; pages that do not parse as a recipe are reported as failures and
//! skipped. Deduplication is by normalized source URL, so repeated scrapes only
//! import recipes not already stored.

use super::url::{fetch_html, UrlSource};
use super::RecipeSourceIngest;
use crate::domain::{Recipe, RecipeSource};
use anyhow::{Context, Result};
use scraper::{Html, Selector};
use std::collections::HashSet;
use std::time::Duration;
use url::Url;

/// Fetches page HTML for a URL. Abstracted so scraping is testable offline.
pub trait HtmlFetcher {
    fn fetch(&self, url: &str) -> Result<String>;
}

/// Live fetcher backed by a blocking HTTP client.
pub struct HttpFetcher {
    pub timeout: Duration,
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl HtmlFetcher for HttpFetcher {
    fn fetch(&self, url: &str) -> Result<String> {
        fetch_html(url, self.timeout)
    }
}

/// Canonical form of a URL for deduplication: lowercased scheme/host, no
/// fragment, no trailing slash. Query string is preserved.
pub fn normalize_url(u: &str) -> String {
    match Url::parse(u) {
        Ok(url) => {
            let scheme = url.scheme().to_lowercase();
            let host = url.host_str().unwrap_or("").to_lowercase();
            let path = url.path().trim_end_matches('/');
            let path = if path.is_empty() { "/" } else { path };
            let mut s = format!("{scheme}://{host}{path}");
            if let Some(q) = url.query() {
                s.push('?');
                s.push_str(q);
            }
            s
        }
        Err(_) => u.trim().trim_end_matches('/').to_lowercase(),
    }
}

/// The source URL a recipe was ingested from, if any.
pub fn recipe_source_url(r: &Recipe) -> Option<String> {
    match &r.source {
        RecipeSource::Url { url } => Some(url.clone()),
        _ => r.meta.source_url.clone(),
    }
}

/// Extract candidate recipe links from an index page: same-host URLs whose path
/// is a strict descendant of the parent page's path. Order-preserving, deduped.
pub fn discover_recipe_links(base_url: &str, html: &str) -> Vec<String> {
    let Ok(base) = Url::parse(base_url) else {
        return vec![];
    };
    let base_host = base.host_str().map(str::to_lowercase);
    let base_path = base.path().trim_end_matches('/').to_string();

    let doc = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for el in doc.select(&selector) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let Ok(abs) = base.join(href) else {
            continue;
        };
        if !matches!(abs.scheme(), "http" | "https") {
            continue;
        }
        if abs.host_str().map(str::to_lowercase) != base_host {
            continue;
        }
        let path = abs.path().trim_end_matches('/');
        let is_descendant = if base_path.is_empty() {
            !path.is_empty() && path != "/"
        } else {
            path.len() > base_path.len()
                && path.starts_with(&base_path)
                && path.as_bytes().get(base_path.len()) == Some(&b'/')
        };
        if !is_descendant {
            continue;
        }
        let mut clean = abs.clone();
        clean.set_fragment(None);
        let link = clean.as_str().trim_end_matches('/').to_string();
        if seen.insert(normalize_url(&link)) {
            out.push(link);
        }
    }
    out
}

/// Recipes gathered by a scrape plus counts for reporting.
#[derive(Debug, Default)]
pub struct ScrapeOutcome {
    pub recipes: Vec<Recipe>,
    /// Total recipe-like links found on the index page.
    pub candidates: usize,
    /// Links skipped because a recipe with that URL is already stored.
    pub skipped_existing: usize,
    /// Links that could not be fetched or parsed as a recipe: (url, error).
    pub failed: Vec<(String, String)>,
}

/// Crawl `base_url`, ingesting up to `limit` recipes whose normalized URL is not
/// already in `existing`. Pages that do not parse as recipes are recorded in
/// `failed` and skipped.
pub fn scrape_new_recipes(
    fetcher: &dyn HtmlFetcher,
    base_url: &str,
    limit: usize,
    existing: &HashSet<String>,
) -> Result<ScrapeOutcome> {
    let index_html = fetcher
        .fetch(base_url)
        .with_context(|| format!("fetching index page {base_url}"))?;
    let links = discover_recipe_links(base_url, &index_html);

    let mut outcome = ScrapeOutcome {
        candidates: links.len(),
        ..Default::default()
    };
    let mut seen = HashSet::new();

    for link in links {
        if outcome.recipes.len() >= limit {
            break;
        }
        let norm = normalize_url(&link);
        if !seen.insert(norm.clone()) {
            continue;
        }
        if existing.contains(&norm) {
            outcome.skipped_existing += 1;
            continue;
        }
        match fetcher.fetch(&link) {
            Ok(html) => {
                let source = UrlSource {
                    offline_html: Some(html),
                    ..Default::default()
                };
                match source.ingest(&link) {
                    Ok(recipe) => outcome.recipes.push(recipe),
                    Err(e) => outcome.failed.push((link, e.to_string())),
                }
            }
            Err(e) => outcome.failed.push((link, e.to_string())),
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MapFetcher {
        pages: HashMap<String, String>,
    }

    impl HtmlFetcher for MapFetcher {
        fn fetch(&self, url: &str) -> Result<String> {
            self.pages
                .get(url)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no page for {url}"))
        }
    }

    fn recipe_html(name: &str) -> String {
        format!(
            r#"<html><head><script type="application/ld+json">
            {{"@context":"https://schema.org","@type":"Recipe","name":"{name}",
              "recipeIngredient":["1 cup flour","2 eggs"],
              "recipeInstructions":"Mix."}}
            </script></head><body></body></html>"#
        )
    }

    fn index_html() -> &'static str {
        r#"<html><body>
            <a href="/recipes/apple-pie">Apple Pie</a>
            <a href="/recipes/banana-bread/">Banana Bread</a>
            <a href="https://site.com/recipes/carrot-cake#top">Carrot Cake</a>
            <a href="/recipes/apple-pie">dup</a>
            <a href="/about">About</a>
            <a href="/recipes">Index itself</a>
            <a href="https://other.com/recipes/stew">External</a>
        </body></html>"#
    }

    #[test]
    fn discovers_only_child_recipe_links() {
        let links = discover_recipe_links("https://site.com/recipes", index_html());
        assert_eq!(
            links,
            vec![
                "https://site.com/recipes/apple-pie",
                "https://site.com/recipes/banana-bread",
                "https://site.com/recipes/carrot-cake",
            ]
        );
    }

    #[test]
    fn normalize_url_canonicalizes() {
        assert_eq!(
            normalize_url("HTTPS://Site.com/Recipes/A/"),
            "https://site.com/Recipes/A"
        );
        assert_eq!(
            normalize_url("https://site.com/recipes/a#frag"),
            "https://site.com/recipes/a"
        );
    }

    fn fetcher() -> MapFetcher {
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            index_html().to_string(),
        );
        pages.insert(
            "https://site.com/recipes/apple-pie".to_string(),
            recipe_html("Apple Pie"),
        );
        pages.insert(
            "https://site.com/recipes/banana-bread".to_string(),
            recipe_html("Banana Bread"),
        );
        pages.insert(
            "https://site.com/recipes/carrot-cake".to_string(),
            recipe_html("Carrot Cake"),
        );
        MapFetcher { pages }
    }

    #[test]
    fn scrapes_all_new_recipes() {
        let out = scrape_new_recipes(&fetcher(), "https://site.com/recipes", 10, &HashSet::new())
            .unwrap();
        assert_eq!(out.candidates, 3);
        assert_eq!(out.recipes.len(), 3);
        assert_eq!(out.skipped_existing, 0);
        assert!(out.failed.is_empty());
    }

    #[test]
    fn second_scrape_skips_already_stored() {
        let existing: HashSet<String> = ["https://site.com/recipes/apple-pie"]
            .iter()
            .map(|u| normalize_url(u))
            .collect();
        let out =
            scrape_new_recipes(&fetcher(), "https://site.com/recipes", 10, &existing).unwrap();
        assert_eq!(out.recipes.len(), 2);
        assert_eq!(out.skipped_existing, 1);
        assert!(out.recipes.iter().all(|r| r.title != "Apple Pie"));
    }

    #[test]
    fn respects_limit() {
        let out =
            scrape_new_recipes(&fetcher(), "https://site.com/recipes", 1, &HashSet::new()).unwrap();
        assert_eq!(out.recipes.len(), 1);
    }

    #[test]
    fn non_recipe_pages_reported_as_failed() {
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<a href="/recipes/not-a-recipe">x</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/not-a-recipe".to_string(),
            "<html><body>no recipe here</body></html>".to_string(),
        );
        let f = MapFetcher { pages };
        let out = scrape_new_recipes(&f, "https://site.com/recipes", 10, &HashSet::new()).unwrap();
        assert_eq!(out.recipes.len(), 0);
        assert_eq!(out.failed.len(), 1);
    }
}
