//! DuckDuckGo HTML search → multi-host recipe crawl.
//!
//! Search engine fetches are separate from the crawl budget (`--limit` only counts
//! candidate recipe/site pages). Result links are unwrapped from DDG's `uddg=`
//! redirect targets and fed into [`super::crawl::scrape_from_seeds`].

use super::crawl::{
    is_denied_url, normalize_url, scrape_from_seeds, HtmlFetcher, ScrapeEvent, ScrapeOutcome,
    ScrapeParams,
};
use anyhow::{bail, Context, Result};
use scraper::{Html, Selector};
use std::collections::HashSet;
use std::time::Duration;
use url::Url;

const DDG_HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
/// Approximate organic results per DDG HTML page (used for `s=` offset).
const DDG_PAGE_SIZE: usize = 30;
/// Pause between SERP page fetches.
const DDG_PAGE_DELAY: Duration = Duration::from_millis(300);

/// Build a DuckDuckGo HTML search URL for `query` at zero-based `page_index`.
pub fn duckduckgo_search_url(query: &str, page_index: usize) -> String {
    let mut url = Url::parse(DDG_HTML_ENDPOINT).expect("static DDG URL");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query);
        if page_index > 0 {
            pairs.append_pair("s", &(page_index * DDG_PAGE_SIZE).to_string());
        }
    }
    url.into()
}

/// True when the response is DDG's bot/anomaly challenge page.
pub fn is_duckduckgo_challenge(html: &str) -> bool {
    html.contains("anomaly-modal")
        || html.contains("Unfortunately, bots use DuckDuckGo")
        || html.contains("cc=botnet")
}

/// Unwrap a DDG redirect (`//duckduckgo.com/l/?uddg=…`) or return the URL as-is.
pub fn unwrap_ddg_redirect(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
        return None;
    }

    let abs = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    };

    let Ok(url) = Url::parse(&abs) else {
        return None;
    };

    if let Some(pairs) = url.query() {
        for (k, v) in url::form_urlencoded::parse(pairs.as_bytes()) {
            if k == "uddg" {
                let target = v.into_owned();
                if target.starts_with("http://") || target.starts_with("https://") {
                    return sanitize_result_url(&target);
                }
            }
        }
    }

    sanitize_result_url(url.as_str())
}

/// Host is exactly `domain` or a subdomain of it (boundary-safe).
fn host_is_or_subdomain(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn sanitize_result_url(raw: &str) -> Option<String> {
    let Ok(mut url) = Url::parse(raw) else {
        return None;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    // Drop DDG itself, ad click trackers, and obvious junk (host-boundary match).
    if host_is_or_subdomain(&host, "duckduckgo.com")
        || host_is_or_subdomain(&host, "bing.com")
        || host_is_or_subdomain(&host, "google.com")
        || host_is_or_subdomain(&host, "googleadservices.com")
    {
        return None;
    }
    let path = url.path().to_lowercase();
    if path.contains("/y.js") || path.contains("/aclick") {
        return None;
    }
    url.set_fragment(None);
    let s = url.as_str().trim_end_matches('/').to_string();
    if is_denied_url(&s) {
        return None;
    }
    Some(s)
}

/// Parse organic result URLs from a DuckDuckGo HTML SERP body.
pub fn parse_duckduckgo_results(html: &str) -> Result<Vec<String>> {
    if is_duckduckgo_challenge(html) {
        bail!("DuckDuckGo returned a bot challenge page; try again later or reduce request rate");
    }

    let doc = Html::parse_document(html);
    let sel_primary = Selector::parse("a.result__a").expect("static selector");
    let sel_fallback = Selector::parse("a[href*='uddg=']").expect("static selector");

    let mut out = Vec::new();
    let mut seen = HashSet::new();

    let push_from =
        |el: scraper::ElementRef<'_>, out: &mut Vec<String>, seen: &mut HashSet<String>| {
            let Some(href) = el.value().attr("href") else {
                return;
            };
            let Some(target) = unwrap_ddg_redirect(href) else {
                return;
            };
            let n = normalize_url(&target);
            if seen.insert(n) {
                out.push(target);
            }
        };

    for el in doc.select(&sel_primary) {
        push_from(el, &mut out, &mut seen);
    }
    // Fallback when primary selector matches nothing *or* only junk (ads/deny).
    if out.is_empty() {
        for el in doc.select(&sel_fallback) {
            push_from(el, &mut out, &mut seen);
        }
    }

    Ok(out)
}

/// Fetch up to `pages` DDG HTML result pages and return deduped destination URLs.
///
/// Page 0 hard-fails on network/challenge errors.
/// Later pages soft-fail: log-level break and return seeds already collected.
pub fn search_result_urls(
    fetcher: &dyn HtmlFetcher,
    query: &str,
    pages: usize,
) -> Result<Vec<String>> {
    search_result_urls_with_delay(fetcher, query, pages, DDG_PAGE_DELAY)
}

fn search_result_urls_with_delay(
    fetcher: &dyn HtmlFetcher,
    query: &str,
    pages: usize,
    page_delay: Duration,
) -> Result<Vec<String>> {
    let pages = pages.max(1);
    let mut all = Vec::new();
    let mut seen = HashSet::new();

    for page in 0..pages {
        if page > 0 && page_delay > Duration::ZERO {
            std::thread::sleep(page_delay);
        }
        let search_url = duckduckgo_search_url(query, page);
        let html = match fetcher.fetch(&search_url) {
            Ok(h) => h,
            Err(e) if page == 0 => {
                return Err(e).with_context(|| {
                    format!("fetching DuckDuckGo results for '{query}' (page {page})")
                });
            }
            Err(_) => break,
        };
        let found = match parse_duckduckgo_results(&html) {
            Ok(f) => f,
            Err(e) if page == 0 => {
                return Err(e).context(format!(
                    "parsing DuckDuckGo results for '{query}' (page {page})"
                ));
            }
            // Later-page challenge/error: keep seeds already collected.
            Err(_) => break,
        };
        if page == 0 && found.is_empty() {
            // Empty first page with real HTML — not an error; just no results.
            break;
        }
        let mut new_on_page = 0usize;
        for u in found {
            let n = normalize_url(&u);
            if seen.insert(n) {
                all.push(u);
                new_on_page += 1;
            }
        }
        // Stop early if a later page adds nothing new.
        if page > 0 && new_on_page == 0 {
            break;
        }
    }

    Ok(all)
}

/// Search DuckDuckGo for `query`, then multi-host BFS-crawl result URLs.
pub fn search_scrape_recipes(
    fetcher: &dyn HtmlFetcher,
    query: &str,
    skip: &HashSet<String>,
    params: ScrapeParams,
    search_pages: usize,
    progress: &(dyn Fn(ScrapeEvent) + Sync),
) -> Result<ScrapeOutcome> {
    let seeds = search_result_urls(fetcher, query, search_pages)?;
    scrape_from_seeds(fetcher, &seeds, skip, params, progress, HashSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::crawl::HtmlFetcher;
    use std::collections::HashMap;
    use std::sync::Mutex;

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

    fn sample_ddg_html() -> &'static str {
        r#"<!DOCTYPE html><html><body>
        <div class="results">
          <div class="result web-result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.allrecipes.com%2Frecipes%2F78%2Fbreakfast%2F&amp;rut=abc">Breakfast</a>
          </div>
          <div class="result web-result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fnatashaskitchen.com%2Fpancakes%2F&amp;rut=def">Pancakes</a>
          </div>
          <div class="result web-result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fduckduckgo.com%2Fy.js%3Fad_domain%3Dx.com&amp;rut=ad">Ad</a>
          </div>
          <div class="result web-result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fabout&amp;rut=ghi">About</a>
          </div>
        </div>
        </body></html>"#
    }

    #[test]
    fn parses_organic_results_unwraps_uddg_filters_junk() {
        let urls = parse_duckduckgo_results(sample_ddg_html()).unwrap();
        assert_eq!(
            urls,
            vec![
                "https://www.allrecipes.com/recipes/78/breakfast".to_string(),
                "https://natashaskitchen.com/pancakes".to_string(),
            ]
        );
    }

    #[test]
    fn challenge_page_is_error() {
        let html = r#"<div class="anomaly-modal">Unfortunately, bots use DuckDuckGo too.</div>"#;
        assert!(is_duckduckgo_challenge(html));
        assert!(parse_duckduckgo_results(html).is_err());
    }

    #[test]
    fn search_url_encodes_query_and_offset() {
        let u0 = duckduckgo_search_url("best meal prep", 0);
        assert!(u0.contains("html.duckduckgo.com/html/"));
        assert!(u0.contains("q=best+meal+prep") || u0.contains("q=best%20meal%20prep"));
        assert!(!u0.contains("s="));
        let u1 = duckduckgo_search_url("best meal prep", 1);
        assert!(u1.contains("s=30"));
    }

    #[test]
    fn sanitize_host_uses_domain_boundaries() {
        assert!(sanitize_result_url("https://www.bing.com/search?q=x").is_none());
        assert!(sanitize_result_url("https://google.com/search").is_none());
        assert!(sanitize_result_url("https://www.google.com/search").is_none());
        assert!(sanitize_result_url("https://duckduckgo.com/y.js?ad=1").is_none());
        // Must not false-positive on hosts that merely contain the substring.
        assert!(sanitize_result_url("https://notgoogle.com/recipe").is_some());
        assert!(sanitize_result_url("https://foobing.com/recipe").is_some());
        assert!(sanitize_result_url("https://natashaskitchen.com/pancakes").is_some());
    }

    #[test]
    fn fallback_used_when_primary_only_yields_junk() {
        // Primary result__a yields only ads/denied; fallback runs on remaining uddg anchors.
        let html = r#"<!DOCTYPE html><html><body>
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fduckduckgo.com%2Fy.js%3Fx=1">Ad</a>
          <a href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fgoodsite.com%2Fpancakes">Alt</a>
        </body></html>"#;
        let urls = parse_duckduckgo_results(html).unwrap();
        assert_eq!(urls, vec!["https://goodsite.com/pancakes".to_string()]);
    }

    #[test]
    fn page0_challenge_errors_but_later_page_keeps_seeds() {
        let mut pages = HashMap::new();
        pages.insert(
            duckduckgo_search_url("breakfast recipes", 0),
            sample_ddg_html().to_string(),
        );
        pages.insert(
            duckduckgo_search_url("breakfast recipes", 1),
            r#"<div class="anomaly-modal">Unfortunately, bots use DuckDuckGo too.</div>"#
                .to_string(),
        );
        let f = MapFetcher { pages };
        let urls =
            search_result_urls_with_delay(&f, "breakfast recipes", 2, Duration::ZERO).unwrap();
        assert_eq!(
            urls,
            vec![
                "https://www.allrecipes.com/recipes/78/breakfast".to_string(),
                "https://natashaskitchen.com/pancakes".to_string(),
            ]
        );
    }

    #[test]
    fn empty_first_page_returns_empty_ok() {
        let mut pages = HashMap::new();
        pages.insert(
            duckduckgo_search_url("zzzz-no-results", 0),
            r#"<!DOCTYPE html><html><body><div class="results"></div></body></html>"#.to_string(),
        );
        let f = MapFetcher { pages };
        let urls = search_result_urls_with_delay(&f, "zzzz-no-results", 2, Duration::ZERO).unwrap();
        assert!(urls.is_empty());
    }

    #[test]
    fn pagination_stops_when_no_new_urls() {
        let ddg = sample_ddg_html().to_string();
        let mut pages = HashMap::new();
        pages.insert(duckduckgo_search_url("overlap", 0), ddg.clone());
        // Page 1 returns the same organic URLs → no new; should stop cleanly.
        pages.insert(duckduckgo_search_url("overlap", 1), ddg);
        let f = MapFetcher { pages };
        let urls = search_result_urls_with_delay(&f, "overlap", 3, Duration::ZERO).unwrap();
        assert_eq!(urls.len(), 2);
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

    #[test]
    fn search_scrape_multi_host_bfs() {
        let ddg = sample_ddg_html().to_string();
        let mut pages = HashMap::new();
        pages.insert(duckduckgo_search_url("breakfast recipes", 0), ddg);
        pages.insert(
            "https://www.allrecipes.com/recipes/78/breakfast".to_string(),
            r#"<html><body>
                <a href="/recipe/omelette">Omelette</a>
                <a href="/recipe/waffles">Waffles</a>
                </body></html>"#
                .to_string(),
        );
        pages.insert(
            "https://www.allrecipes.com/recipe/omelette".to_string(),
            recipe_html("Omelette"),
        );
        pages.insert(
            "https://www.allrecipes.com/recipe/waffles".to_string(),
            recipe_html("Waffles"),
        );
        pages.insert(
            "https://natashaskitchen.com/pancakes".to_string(),
            recipe_html("Pancakes"),
        );
        let f = MapFetcher { pages };
        let planned = Mutex::new(0usize);
        let out = search_scrape_recipes(
            &f,
            "breakfast recipes",
            &HashSet::new(),
            ScrapeParams::new_fast(20, 4, 2),
            1,
            &|e| {
                if matches!(e, ScrapeEvent::Planned { .. }) {
                    *planned.lock().unwrap() += 1;
                }
            },
        )
        .unwrap();
        // search_scrape_recipes must not pre-emit Planned; only the crawl does. At least one is required.
        assert!(*planned.lock().unwrap() >= 1);
        let titles: HashSet<_> = out.recipes.iter().map(|r| r.title.as_str()).collect();
        assert!(
            titles.contains("Pancakes"),
            "direct seed recipe: {titles:?}"
        );
        assert!(
            titles.contains("Omelette"),
            "expanded host recipe: {titles:?}"
        );
        assert!(
            titles.contains("Waffles"),
            "expanded host recipe: {titles:?}"
        );
    }
}
