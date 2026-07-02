//! Crawl an index/parent page for recipe links and ingest new ones.
//!
//! Discovery follows the pattern "a parent page like `site.com/recipes` links to
//! child recipe pages like `site.com/recipes/some-dish`": links are kept when they
//! are same-host descendants of the **seed** path (not merely the current page).
//! With `max_depth > 1`, non-seed pages that were fetched are themselves scanned
//! for further descendant links (BFS), so category/index trees are walked
//! recursively. Each candidate is parsed with [`UrlSource`]; pages that do not
//! parse as a recipe are reported as failures so callers can remember them.
//! Candidate pages are fetched concurrently (per BFS frontier batch) and
//! deduplicated by normalized source URL.

use super::url::UrlSource;
use super::RecipeSourceIngest;
use crate::domain::{Recipe, RecipeSource};
use anyhow::{Context, Result};
use scraper::{Html, Selector};
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;
use url::Url;

/// Fetches page HTML for a URL. Abstracted so scraping is testable offline.
/// Implementations must be `Sync` so candidates can be fetched concurrently.
pub trait HtmlFetcher: Sync {
    fn fetch(&self, url: &str) -> Result<String>;
}

/// Live fetcher backed by a single reusable blocking HTTP client.
pub struct HttpFetcher {
    client: reqwest::blocking::Client,
}

impl Default for HttpFetcher {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("smarter-recipes/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build blocking HTTP client");
        Self { client }
    }
}

impl HtmlFetcher for HttpFetcher {
    fn fetch(&self, url: &str) -> Result<String> {
        let resp = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("fetching {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {} fetching {url}", resp.status());
        }
        resp.text().context("reading response body")
    }
}

/// Progress emitted during a scrape. Handlers run on worker threads, so they must
/// be cheap and thread-safe (e.g. line-buffered `eprintln!`).
#[derive(Debug, Clone)]
pub enum ScrapeEvent {
    /// Emitted after the seed page is scanned (and when recursion expands the plan).
    Planned {
        candidates: usize,
        skipped: usize,
        to_fetch: usize,
    },
    /// A candidate page parsed into a recipe.
    Imported { url: String, title: String },
    /// A candidate page could not be fetched or parsed as a recipe.
    Failed { url: String, reason: String },
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

fn is_path_descendant(path: &str, root_path: &str) -> bool {
    let path = path.trim_end_matches('/');
    let root_path = root_path.trim_end_matches('/');
    if root_path.is_empty() || root_path == "/" {
        return !path.is_empty() && path != "/";
    }
    path.len() > root_path.len()
        && path.starts_with(root_path)
        && path.as_bytes().get(root_path.len()) == Some(&b'/')
}

/// Extract links from `page_url`'s HTML that stay under the seed `root_url`
/// (same host, path strict descendant of the seed path). Order-preserving, deduped.
pub fn discover_scoped_links(page_url: &str, html: &str, root_url: &str) -> Vec<String> {
    let Ok(page) = Url::parse(page_url) else {
        return vec![];
    };
    let Ok(root) = Url::parse(root_url) else {
        return vec![];
    };
    let root_host = root.host_str().map(str::to_lowercase);
    let root_path = root.path().trim_end_matches('/').to_string();

    let doc = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for el in doc.select(&selector) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let Ok(abs) = page.join(href) else {
            continue;
        };
        if !matches!(abs.scheme(), "http" | "https") {
            continue;
        }
        if abs.host_str().map(str::to_lowercase) != root_host {
            continue;
        }
        if !is_path_descendant(abs.path(), &root_path) {
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

/// Extract candidate recipe links from an index page: same-host URLs whose path
/// is a strict descendant of the parent page's path. Order-preserving, deduped.
pub fn discover_recipe_links(base_url: &str, html: &str) -> Vec<String> {
    discover_scoped_links(base_url, html, base_url)
}

/// Recipes gathered by a scrape plus counts for reporting.
#[derive(Debug, Default)]
pub struct ScrapeOutcome {
    pub recipes: Vec<Recipe>,
    /// Total unique descendant links discovered during the crawl (all depths).
    pub candidates: usize,
    /// Links skipped because their URL was already known (imported or failed).
    pub skipped_existing: usize,
    /// Links that could not be fetched or parsed as a recipe: (url, error).
    pub failed: Vec<(String, String)>,
}

fn fetch_html(fetcher: &dyn HtmlFetcher, url: &str) -> Result<String, String> {
    fetcher.fetch(url).map_err(|e| e.to_string())
}

fn ingest_html(url: &str, html: String) -> Result<Recipe, String> {
    let source = UrlSource {
        offline_html: Some(html),
        ..Default::default()
    };
    source.ingest(url).map_err(|e| e.to_string())
}

/// Crawl `base_url` and BFS-fetch up to `limit` new pages under the seed path.
///
/// - Depth `1` (default): only links found on the seed page (previous behavior).
/// - Depth `N`: also scan fetched pages for further descendant links, up to
///   depth `N` from the seed.
///
/// `limit` bounds non-seed page fetches, not successes. Pages are fetched
/// `jobs` at a time per frontier batch. Failures go in `failed` (discovery order).
pub fn scrape_new_recipes(
    fetcher: &dyn HtmlFetcher,
    base_url: &str,
    limit: usize,
    skip: &HashSet<String>,
    jobs: usize,
    max_depth: usize,
    progress: &(dyn Fn(ScrapeEvent) + Sync),
) -> Result<ScrapeOutcome> {
    let max_depth = max_depth.max(1);
    let jobs = jobs.max(1);
    let root_norm = normalize_url(base_url);

    let seed_html = fetcher
        .fetch(base_url)
        .with_context(|| format!("fetching index page {base_url}"))?;

    // BFS: (url, depth). Depth 0 is the seed (already fetched).
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut enqueued: HashSet<String> = HashSet::new();
    enqueued.insert(root_norm.clone());

    let mut candidates = 0usize;
    let mut skipped_existing = 0usize;
    let mut recipes: Vec<Recipe> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut fetches_used = 0usize;

    // Seed links at depth 1.
    for link in discover_scoped_links(base_url, &seed_html, base_url) {
        let n = normalize_url(&link);
        if !enqueued.insert(n.clone()) {
            continue;
        }
        candidates += 1;
        if skip.contains(&n) {
            skipped_existing += 1;
            continue;
        }
        if max_depth >= 1 {
            queue.push_back((link, 1));
        }
    }

    progress(ScrapeEvent::Planned {
        candidates,
        skipped: skipped_existing,
        to_fetch: queue.len().min(limit.saturating_sub(fetches_used)),
    });

    while !queue.is_empty() && fetches_used < limit {
        // Take a batch from the front of the frontier (stable BFS order).
        let mut batch: Vec<(String, usize)> = Vec::new();
        while batch.len() < jobs && fetches_used + batch.len() < limit {
            let Some(item) = queue.pop_front() else {
                break;
            };
            batch.push(item);
        }
        if batch.is_empty() {
            break;
        }

        // batch_idx, url, depth, html (empty on fetch err), ingest outcome
        type BatchRow = (usize, String, usize, String, Result<Recipe, String>);
        let results: Mutex<Vec<BatchRow>> = Mutex::new(Vec::new());

        std::thread::scope(|s| {
            for (bi, (url, depth)) in batch.iter().enumerate() {
                let results = &results;
                s.spawn(move || {
                    let html = match fetch_html(fetcher, url) {
                        Ok(h) => h,
                        Err(reason) => {
                            progress(ScrapeEvent::Failed {
                                url: url.clone(),
                                reason: reason.clone(),
                            });
                            results.lock().unwrap().push((
                                bi,
                                url.clone(),
                                *depth,
                                String::new(),
                                Err(reason),
                            ));
                            return;
                        }
                    };
                    let ingest = ingest_html(url, html.clone());
                    match &ingest {
                        Ok(recipe) => progress(ScrapeEvent::Imported {
                            url: url.clone(),
                            title: recipe.title.clone(),
                        }),
                        Err(reason) => progress(ScrapeEvent::Failed {
                            url: url.clone(),
                            reason: reason.clone(),
                        }),
                    }
                    results
                        .lock()
                        .unwrap()
                        .push((bi, url.clone(), *depth, html, ingest));
                });
            }
        });

        let mut batch_results = results.into_inner().unwrap();
        batch_results.sort_by_key(|(bi, _, _, _, _)| *bi);

        for (_bi, url, depth, html, ingest) in batch_results {
            fetches_used += 1;
            match ingest {
                Ok(recipe) => recipes.push(recipe),
                Err(reason) => failed.push((url.clone(), reason)),
            }

            // Recurse: scan page for more descendant links when depth allows.
            if depth < max_depth && !html.is_empty() {
                let mut new_queued = 0usize;
                for link in discover_scoped_links(&url, &html, base_url) {
                    let n = normalize_url(&link);
                    if !enqueued.insert(n.clone()) {
                        continue;
                    }
                    candidates += 1;
                    if skip.contains(&n) {
                        skipped_existing += 1;
                        continue;
                    }
                    queue.push_back((link, depth + 1));
                    new_queued += 1;
                }
                if new_queued > 0 {
                    progress(ScrapeEvent::Planned {
                        candidates,
                        skipped: skipped_existing,
                        to_fetch: queue.len().min(limit.saturating_sub(fetches_used)),
                    });
                }
            }
        }
    }

    Ok(ScrapeOutcome {
        recipes,
        candidates,
        skipped_existing,
        failed,
    })
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
    fn scoped_links_use_root_not_page_path() {
        // Nested index under /recipes; links must still be under seed /recipes.
        let html = r#"<a href="/recipes/italian/pasta">Pasta</a>
                      <a href="/blog/post">Blog</a>"#;
        let links = discover_scoped_links(
            "https://site.com/recipes/italian",
            html,
            "https://site.com/recipes",
        );
        assert_eq!(links, vec!["https://site.com/recipes/italian/pasta"]);
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

    fn noop(_: ScrapeEvent) {}

    fn scrape(
        fetcher: &dyn HtmlFetcher,
        base: &str,
        limit: usize,
        skip: &HashSet<String>,
        depth: usize,
    ) -> ScrapeOutcome {
        scrape_new_recipes(fetcher, base, limit, skip, 4, depth, &noop).unwrap()
    }

    #[test]
    fn scrapes_all_new_recipes_in_order() {
        let out = scrape(
            &fetcher(),
            "https://site.com/recipes",
            10,
            &HashSet::new(),
            1,
        );
        assert_eq!(out.candidates, 3);
        let titles: Vec<_> = out.recipes.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["Apple Pie", "Banana Bread", "Carrot Cake"]);
        assert_eq!(out.skipped_existing, 0);
        assert!(out.failed.is_empty());
    }

    #[test]
    fn second_scrape_skips_already_known() {
        let skip: HashSet<String> = ["https://site.com/recipes/apple-pie"]
            .iter()
            .map(|u| normalize_url(u))
            .collect();
        let out = scrape(&fetcher(), "https://site.com/recipes", 10, &skip, 1);
        assert_eq!(out.recipes.len(), 2);
        assert_eq!(out.skipped_existing, 1);
        assert!(out.recipes.iter().all(|r| r.title != "Apple Pie"));
    }

    #[test]
    fn limit_bounds_number_of_fetches() {
        let out = scrape(
            &fetcher(),
            "https://site.com/recipes",
            1,
            &HashSet::new(),
            1,
        );
        assert_eq!(out.recipes.len(), 1);
    }

    #[test]
    fn limit_counts_fetches_including_failures() {
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<html><body>
                <a href="/recipes/broken">broken</a>
                <a href="/recipes/apple-pie">Apple Pie</a>
                <a href="/recipes/banana-bread">Banana Bread</a>
            </body></html>"#
                .to_string(),
        );
        pages.insert(
            "https://site.com/recipes/broken".to_string(),
            "<html><body>no recipe</body></html>".to_string(),
        );
        pages.insert(
            "https://site.com/recipes/apple-pie".to_string(),
            recipe_html("Apple Pie"),
        );
        pages.insert(
            "https://site.com/recipes/banana-bread".to_string(),
            recipe_html("Banana Bread"),
        );
        let f = MapFetcher { pages };
        let out = scrape(&f, "https://site.com/recipes", 2, &HashSet::new(), 1);
        assert_eq!(out.recipes.len(), 1);
        assert_eq!(out.failed.len(), 1);
        assert!(out.recipes.iter().all(|r| r.title != "Banana Bread"));
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
        let out = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 1);
        assert_eq!(out.recipes.len(), 0);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, "https://site.com/recipes/not-a-recipe");
    }

    #[test]
    fn recursion_follows_nested_index() {
        // Seed → category (not a recipe) → leaf recipe. depth=1 misses leaf; depth=2 finds it.
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<a href="/recipes/italian">Italian</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/italian".to_string(),
            r#"<html><body><a href="/recipes/italian/pasta">Pasta</a></body></html>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/italian/pasta".to_string(),
            recipe_html("Pasta"),
        );
        let f = MapFetcher { pages };

        let shallow = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 1);
        assert_eq!(shallow.recipes.len(), 0);
        assert_eq!(shallow.failed.len(), 1); // italian category failed as recipe

        let deep = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 2);
        assert_eq!(deep.recipes.len(), 1);
        assert_eq!(deep.recipes[0].title, "Pasta");
        assert_eq!(deep.candidates, 2); // italian + pasta
    }

    #[test]
    fn recursion_respects_limit_across_depths() {
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<a href="/recipes/cat">Cat</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/cat".to_string(),
            r#"<a href="/recipes/cat/a">A</a><a href="/recipes/cat/b">B</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/cat/a".to_string(),
            recipe_html("A"),
        );
        pages.insert(
            "https://site.com/recipes/cat/b".to_string(),
            recipe_html("B"),
        );
        let f = MapFetcher { pages };
        // limit=2: fetch cat + a, not b
        let out = scrape(&f, "https://site.com/recipes", 2, &HashSet::new(), 2);
        assert_eq!(out.recipes.len(), 1);
        assert_eq!(out.recipes[0].title, "A");
    }

    #[test]
    fn single_job_is_sequential_equivalent() {
        let out = scrape_new_recipes(
            &fetcher(),
            "https://site.com/recipes",
            10,
            &HashSet::new(),
            1,
            1,
            &noop,
        )
        .unwrap();
        assert_eq!(out.recipes.len(), 3);
    }

    #[test]
    fn emits_progress_events() {
        let events = Mutex::new(Vec::new());
        let _ = scrape_new_recipes(
            &fetcher(),
            "https://site.com/recipes",
            10,
            &HashSet::new(),
            4,
            1,
            &|e| events.lock().unwrap().push(e),
        )
        .unwrap();
        let ev = events.into_inner().unwrap();
        assert!(ev.iter().any(|e| matches!(e, ScrapeEvent::Planned { .. })));
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, ScrapeEvent::Imported { .. }))
                .count(),
            3
        );
    }
}
