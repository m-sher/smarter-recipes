//! Crawl a seed page for same-host links and ingest new recipes.
//!
//! # Behavior
//!
//! 1. Fetch the seed URL and collect **same-host** `http(s)` links (not limited to
//!    path descendants of the seed — category pages often link to site-root posts).
//! 2. BFS-fetch candidates up to `--depth` / `max_depth`, scanning each fetched
//!    page for more same-host links when depth allows.
//! 3. Try to parse each non-seed page as a recipe. Pages that are not recipes are
//!    **navigation** (category/index): they expand the frontier but are **not**
//!    recorded as scrape failures, so re-runs can still traverse them to find
//!    newly published recipes.
//! 4. Only **hard** failures (network / HTTP errors) go in [`ScrapeOutcome::failed`]
//!    for persistence. A small deny-list skips obvious non-content paths and assets.
//!
//! Candidates are fetched concurrently per BFS frontier batch and deduplicated by
//! normalized URL.

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
    /// Page fetched OK but is not a recipe (category / nav). Not a persistent failure.
    NotRecipe { url: String, reason: String },
    /// Hard fetch/network failure (safe to persist for future skip).
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

/// Path segments that are almost never recipe content (budget protection).
const DENY_SEGMENTS: &[&str] = &[
    "about", "author", "authors", "tag", "tags", "feed", "feeds", "wp-admin", "wp-login", "cart",
    "checkout", "account", "login", "register", "privacy", "terms", "contact", "search", "comment",
    "comments", "cdn-cgi",
];

const DENY_EXTENSIONS: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".ico", ".css", ".js", ".mjs", ".pdf",
    ".zip", ".xml", ".json", ".mp4", ".mp3", ".woff", ".woff2", ".ttf",
];

/// True when the URL is an obvious non-content / asset target.
pub fn is_denied_url(u: &str) -> bool {
    let Ok(url) = Url::parse(u) else {
        return true;
    };
    let path = url.path().to_lowercase();
    if DENY_EXTENSIONS.iter().any(|ext| path.ends_with(ext)) {
        return true;
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segments.iter().any(|seg| {
        DENY_SEGMENTS
            .iter()
            .any(|d| seg == d || seg.starts_with(&format!("{d}.")))
    })
}

/// True when the URL is a category/tag/pagination/index page that must not
/// be stored as a recipe (JSON-LD on those pages is usually a featured recipe).
/// Still allowed as a BFS node for link discovery.
pub fn is_listing_url(u: &str) -> bool {
    let Ok(url) = Url::parse(u) else {
        return true;
    };
    let path = url.path().to_lowercase();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return true;
    }
    const LISTING_SEGMENTS: &[&str] = &[
        "category",
        "categories",
        "tag",
        "tags",
        "author",
        "authors",
        "collection",
        "collections",
        "roundup",
        "roundups",
        "gallery",
        "galleries",
        "photos",
        "topics",
        "topic",
    ];
    if segments.iter().any(|s| LISTING_SEGMENTS.contains(s)) {
        return true;
    }
    // /page/2 or .../page/2
    segments
        .windows(2)
        .any(|w| w[0] == "page" && w[1].chars().all(|c| c.is_ascii_digit()))
        || (segments.last().is_some_and(|s| *s == "page"))
}

/// Extract same-host `http(s)` links from `page_url`'s HTML, applying the deny-list.
/// Order-preserving, deduped. `root_url` is only used for host scoping (same host as seed).
pub fn discover_scoped_links(page_url: &str, html: &str, root_url: &str) -> Vec<String> {
    let Ok(page) = Url::parse(page_url) else {
        return vec![];
    };
    let Ok(root) = Url::parse(root_url) else {
        return vec![];
    };
    let root_host = root.host_str().map(str::to_lowercase);

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
        let mut clean = abs.clone();
        clean.set_fragment(None);
        let link = clean.as_str().trim_end_matches('/').to_string();
        if is_denied_url(&link) {
            continue;
        }
        // Skip the seed / current page itself when equal after normalize.
        if normalize_url(&link) == normalize_url(page_url)
            || normalize_url(&link) == normalize_url(root_url)
        {
            continue;
        }
        if seen.insert(normalize_url(&link)) {
            out.push(link);
        }
    }
    out
}

/// Extract candidate links from an index page (same-host, deny-listed filtered).
pub fn discover_recipe_links(base_url: &str, html: &str) -> Vec<String> {
    discover_scoped_links(base_url, html, base_url)
}

/// Recipes gathered by a scrape plus counts for reporting.
#[derive(Debug, Default)]
pub struct ScrapeOutcome {
    pub recipes: Vec<Recipe>,
    /// Total unique same-host links discovered during the crawl (all depths).
    pub candidates: usize,
    /// Links skipped because their URL was already known (imported or hard-failed).
    pub skipped_existing: usize,
    /// Hard fetch/network failures — safe to persist for skip on future runs.
    pub failed: Vec<(String, String)>,
    /// Pages that fetched OK but are not recipes (nav/category). **Not** for persistence.
    pub not_recipe: Vec<(String, String)>,
}

/// Shared fetch budget / concurrency for multi-seed and site scrapes.
#[derive(Debug, Clone, Copy)]
pub struct ScrapeParams {
    /// Max page fetches (not counting search SERPs).
    pub limit: usize,
    /// Concurrent fetches per batch.
    pub jobs: usize,
    /// BFS depth (1 = seeds / seed-links only).
    pub max_depth: usize,
}

impl ScrapeParams {
    pub fn new(limit: usize, jobs: usize, max_depth: usize) -> Self {
        Self {
            limit,
            jobs: jobs.max(1),
            max_depth: max_depth.max(1),
        }
    }
}

/// Enqueue a batch of candidates, preserving discovery order within each class.
/// Non-listing (likely recipe) URLs are placed ahead of any already-queued items
/// and ahead of listing/index URLs so `--limit` prefers content over nav.
fn enqueue_candidates(queue: &mut VecDeque<(String, usize)>, links: Vec<(String, usize)>) {
    let mut content = Vec::new();
    let mut listings = Vec::new();
    for (link, depth) in links {
        if is_listing_url(&link) {
            listings.push((link, depth));
        } else {
            content.push((link, depth));
        }
    }
    // push_front reverses; feed content in reverse so pop_front yields discovery order.
    for item in content.into_iter().rev() {
        queue.push_front(item);
    }
    for item in listings {
        queue.push_back(item);
    }
}

fn fetch_html(fetcher: &dyn HtmlFetcher, url: &str) -> Result<String, String> {
    fetcher.fetch(url).map_err(|e| e.to_string())
}

fn ingest_html(url: &str, html: &str) -> Result<Recipe, String> {
    let source = UrlSource {
        offline_html: Some(html.to_string()),
        ..Default::default()
    };
    source.ingest(url).map_err(|e| e.to_string())
}

/// Crawl `base_url` and BFS-fetch up to `limit` new same-host pages.
///
/// - Depth `1` (default): only links found on the seed page.
/// - Depth `N`: also scan fetched pages for further same-host links, up to depth `N`.
///
/// The seed itself is used only for link discovery (not counted against `limit`).
/// `limit` bounds non-seed page fetches, not successes. Only hard failures are in
/// [`ScrapeOutcome::failed`]; non-recipe pages are in [`ScrapeOutcome::not_recipe`].
pub fn scrape_new_recipes(
    fetcher: &dyn HtmlFetcher,
    base_url: &str,
    limit: usize,
    skip: &HashSet<String>,
    jobs: usize,
    max_depth: usize,
    progress: &(dyn Fn(ScrapeEvent) + Sync),
) -> Result<ScrapeOutcome> {
    let seed_html = fetcher
        .fetch(base_url)
        .with_context(|| format!("fetching index page {base_url}"))?;

    let links = discover_scoped_links(base_url, &seed_html, base_url);
    // Index URL is not a fetch target; mark it seen so we don't loop back to it.
    let mut bootstrap_seen = HashSet::new();
    bootstrap_seen.insert(normalize_url(base_url));
    scrape_from_seeds(
        fetcher,
        &links,
        skip,
        ScrapeParams::new(limit, jobs, max_depth),
        progress,
        bootstrap_seen,
    )
}

/// BFS-fetch `seeds` (each is a page fetch target) up to `params.limit` fetches.
///
/// Unlike [`scrape_new_recipes`], seeds themselves are fetched and may be imported
/// as recipes. Link expansion is **same-host as the page being scanned** so multi-host
/// search results each open their own host frontier under a shared budget.
///
/// - Depth `1`: fetch seeds only (no expansion).
/// - Depth `N`: expand same-host links from fetched pages while `depth < N`.
///
/// `extra_seen` seeds the enqueued set without counting as candidates (e.g. an index
/// URL that was already fetched outside this function).
pub fn scrape_from_seeds(
    fetcher: &dyn HtmlFetcher,
    seeds: &[String],
    skip: &HashSet<String>,
    params: ScrapeParams,
    progress: &(dyn Fn(ScrapeEvent) + Sync),
    extra_seen: HashSet<String>,
) -> Result<ScrapeOutcome> {
    let limit = params.limit;
    let jobs = params.jobs;
    let max_depth = params.max_depth;

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut enqueued: HashSet<String> = extra_seen;

    let mut candidates = 0usize;
    let mut skipped_existing = 0usize;
    let mut recipes: Vec<Recipe> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut not_recipe: Vec<(String, String)> = Vec::new();
    let mut fetches_used = 0usize;

    let mut initial: Vec<(String, usize)> = Vec::new();
    for link in seeds {
        let n = normalize_url(link);
        if !enqueued.insert(n.clone()) {
            continue;
        }
        candidates += 1;
        if skip.contains(&n) {
            skipped_existing += 1;
            continue;
        }
        if is_denied_url(link) {
            continue;
        }
        initial.push((link.clone(), 1));
    }
    enqueue_candidates(&mut queue, initial);

    progress(ScrapeEvent::Planned {
        candidates,
        skipped: skipped_existing,
        to_fetch: queue.len().min(limit.saturating_sub(fetches_used)),
    });

    while !queue.is_empty() && fetches_used < limit {
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

        // batch_idx, url, depth, html (empty on fetch err), fetch_ok, ingest outcome
        type BatchRow = (usize, String, usize, String, bool, Result<Recipe, String>);
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
                                false,
                                Err(reason),
                            ));
                            return;
                        }
                    };
                    let ingest = if is_listing_url(url) {
                        Err("listing/index page".to_string())
                    } else {
                        ingest_html(url, &html)
                    };
                    match &ingest {
                        Ok(recipe) => progress(ScrapeEvent::Imported {
                            url: url.clone(),
                            title: recipe.title.clone(),
                        }),
                        Err(reason) => progress(ScrapeEvent::NotRecipe {
                            url: url.clone(),
                            reason: reason.clone(),
                        }),
                    }
                    results
                        .lock()
                        .unwrap()
                        .push((bi, url.clone(), *depth, html, true, ingest));
                });
            }
        });

        let mut batch_results = results.into_inner().unwrap();
        batch_results.sort_by_key(|(bi, _, _, _, _, _)| *bi);

        for (_bi, url, depth, html, fetch_ok, ingest) in batch_results {
            fetches_used += 1;
            match (fetch_ok, ingest) {
                (true, Ok(recipe)) => recipes.push(recipe),
                (true, Err(reason)) => not_recipe.push((url.clone(), reason)),
                (false, Err(reason)) => failed.push((url.clone(), reason)),
                (false, Ok(_)) => unreachable!("fetch failed cannot produce recipe"),
            }

            // Host-scope expansion to the page's own host (supports multi-host seeds).
            if depth < max_depth && !html.is_empty() {
                let mut batch_links: Vec<(String, usize)> = Vec::new();
                for link in discover_scoped_links(&url, &html, &url) {
                    let n = normalize_url(&link);
                    if !enqueued.insert(n.clone()) {
                        continue;
                    }
                    candidates += 1;
                    if skip.contains(&n) {
                        skipped_existing += 1;
                        continue;
                    }
                    batch_links.push((link, depth + 1));
                }
                let new_queued = batch_links.len();
                if new_queued > 0 {
                    enqueue_candidates(&mut queue, batch_links);
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
        not_recipe,
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
            <a href="/logo.png">Asset</a>
        </body></html>"#
    }

    #[test]
    fn discovers_same_host_recipe_links_skips_deny() {
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
    fn same_host_allows_root_level_posts_from_category_page() {
        // Woks-of-Life style: category seed links to root-level post.
        let html = r#"<a href="/kung-pao-chicken">Kung Pao</a>
                      <a href="/author/bleung">Author</a>"#;
        let links = discover_scoped_links(
            "https://site.com/category/recipes/chicken",
            html,
            "https://site.com/category/recipes/chicken",
        );
        assert_eq!(links, vec!["https://site.com/kung-pao-chicken"]);
    }

    #[test]
    fn denied_paths_and_extensions() {
        assert!(is_denied_url("https://site.com/about"));
        assert!(is_denied_url("https://site.com/author/x"));
        assert!(is_denied_url("https://site.com/foo/bar.jpg"));
        assert!(!is_denied_url("https://site.com/kung-pao-chicken"));
    }

    #[test]
    fn listing_urls_detected() {
        assert!(is_listing_url("https://itsahero.com/category/food"));
        assert!(is_listing_url(
            "https://itsahero.com/category/crafty/recipes/dairy-free/page/2"
        ));
        assert!(is_listing_url("https://site.com/page/2"));
        assert!(is_listing_url("https://site.com/tag/summer"));
        assert!(is_listing_url("https://site.com/"));
    }

    #[test]
    fn recipe_urls_not_listings() {
        assert!(!is_listing_url(
            "https://itsahero.com/chicken-tortellini-skillet"
        ));
        assert!(!is_listing_url(
            "https://itsahero.com/delicious-air-fryer-salsa-verde-recipe"
        ));
    }

    #[test]
    fn listing_page_with_json_ld_is_not_imported_but_links_expand() {
        let mut pages = HashMap::new();
        // Index links only to a category page.
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<a href="/category/food">Food</a>"#.to_string(),
        );
        // Category page has full Recipe JSON-LD AND a link to a real recipe.
        pages.insert(
            "https://site.com/category/food".to_string(),
            r#"<html><body>
              <script type="application/ld+json">{
                "@type":"Recipe","name":"Grilled S'mores",
                "recipeIngredient":["bread","chocolate"]
              }</script>
              <a href="/grilled-smores">real</a>
            </body></html>"#
                .to_string(),
        );
        pages.insert(
            "https://site.com/grilled-smores".to_string(),
            recipe_html("Grilled S'mores"),
        );
        let f = MapFetcher { pages };
        let out = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 2);
        assert!(
            out.recipes.iter().all(|r| {
                recipe_source_url(r)
                    .map(|u| !u.contains("/category/"))
                    .unwrap_or(true)
            }),
            "must not import category URL as recipe source: {:?}",
            out.recipes
                .iter()
                .map(|r| r.title.clone())
                .collect::<Vec<_>>()
        );
        // Real recipe page should still be reachable via BFS.
        assert!(
            out.recipes.iter().any(|r| r.title == "Grilled S'mores"),
            "expected real recipe via expanded link, got {:?}",
            out.recipes.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(
            out.not_recipe
                .iter()
                .any(|(u, _)| u.contains("/category/food")),
            "category page should be classified not_recipe"
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
        assert!(out.not_recipe.is_empty());
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
    fn limit_counts_fetches_including_not_recipe() {
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
        assert_eq!(out.not_recipe.len(), 1);
        assert!(out.failed.is_empty());
        assert!(out.recipes.iter().all(|r| r.title != "Banana Bread"));
    }

    #[test]
    fn non_recipe_pages_are_not_hard_failures() {
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
        assert!(out.failed.is_empty());
        assert_eq!(out.not_recipe.len(), 1);
        assert_eq!(out.not_recipe[0].0, "https://site.com/recipes/not-a-recipe");
    }

    #[test]
    fn hard_fetch_failure_is_recorded() {
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<a href="/recipes/missing">x</a>"#.to_string(),
        );
        // no page for missing → fetch error
        let f = MapFetcher { pages };
        let out = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 1);
        assert_eq!(out.failed.len(), 1);
        assert!(out.not_recipe.is_empty());
        assert!(out.recipes.is_empty());
    }

    #[test]
    fn recursion_follows_nested_index() {
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
        assert_eq!(shallow.not_recipe.len(), 1); // italian is nav, not hard fail
        assert!(shallow.failed.is_empty());

        let deep = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 2);
        assert_eq!(deep.recipes.len(), 1);
        assert_eq!(deep.recipes[0].title, "Pasta");
        assert_eq!(deep.candidates, 2);
    }

    #[test]
    fn cross_run_still_traverses_nav_pages() {
        // Run 1: discover pasta via italian nav. Nav must NOT enter skip via failed.
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<a href="/recipes/italian">Italian</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/italian".to_string(),
            r#"<a href="/recipes/italian/pasta">Pasta</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/italian/pasta".to_string(),
            recipe_html("Pasta"),
        );
        let f = MapFetcher {
            pages: pages.clone(),
        };
        let run1 = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 2);
        assert_eq!(run1.recipes.len(), 1);
        assert!(run1.failed.is_empty());
        // Simulate CLI: only hard failures + imported URLs enter skip.
        let mut skip: HashSet<String> = run1
            .recipes
            .iter()
            .filter_map(recipe_source_url)
            .map(|u| normalize_url(&u))
            .collect();
        for (u, _) in &run1.failed {
            skip.insert(normalize_url(u));
        }
        // Publish pizza under italian for run 2.
        let mut pages2 = pages;
        pages2.insert(
            "https://site.com/recipes/italian".to_string(),
            r#"<a href="/recipes/italian/pasta">Pasta</a>
               <a href="/recipes/italian/pizza">Pizza</a>"#
                .to_string(),
        );
        pages2.insert(
            "https://site.com/recipes/italian/pizza".to_string(),
            recipe_html("Pizza"),
        );
        let f2 = MapFetcher { pages: pages2 };
        let run2 = scrape(&f2, "https://site.com/recipes", 10, &skip, 2);
        assert!(
            run2.recipes.iter().any(|r| r.title == "Pizza"),
            "re-run must find new recipe under previously-seen nav page; got {:?}",
            run2.recipes.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn cycle_links_do_not_loop_forever() {
        let mut pages = HashMap::new();
        pages.insert(
            "https://site.com/recipes".to_string(),
            r#"<a href="/recipes/a">A</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/a".to_string(),
            r#"<a href="/recipes/b">B</a>"#.to_string(),
        );
        pages.insert(
            "https://site.com/recipes/b".to_string(),
            r#"<a href="/recipes/a">A again</a>"#.to_string(),
        );
        let f = MapFetcher { pages };
        let out = scrape(&f, "https://site.com/recipes", 50, &HashSet::new(), 10);
        // Only two non-seed fetches possible; enqueued set stops A↔B explosion.
        assert!(out.recipes.is_empty());
        assert_eq!(out.not_recipe.len(), 2);
        assert!(out.failed.is_empty());
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

    #[test]
    fn multi_seed_fetches_seeds_across_hosts() {
        let mut pages = HashMap::new();
        pages.insert("https://a.com/pie".to_string(), recipe_html("Pie"));
        pages.insert("https://b.com/soup".to_string(), recipe_html("Soup"));
        pages.insert(
            "https://b.com/list".to_string(),
            r#"<a href="/stew">Stew</a>"#.to_string(),
        );
        pages.insert("https://b.com/stew".to_string(), recipe_html("Stew"));
        let f = MapFetcher { pages };
        let seeds = vec![
            "https://a.com/pie".to_string(),
            "https://b.com/list".to_string(),
        ];
        let out = scrape_from_seeds(
            &f,
            &seeds,
            &HashSet::new(),
            ScrapeParams::new(10, 4, 2),
            &noop,
            HashSet::new(),
        )
        .unwrap();
        let titles: HashSet<_> = out.recipes.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.contains("Pie"));
        assert!(titles.contains("Stew"));
        assert!(!titles.contains("Soup")); // never seeded or linked from list-only path used
    }
}
