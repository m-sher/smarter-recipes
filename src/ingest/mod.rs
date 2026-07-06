//! Pluggable recipe ingestion sources.

mod crawl;
mod epub;
mod file;
mod manual;
mod ocr;
mod quality;
mod search;
mod url;

pub use crawl::{
    discover_recipe_links, discover_scoped_links, epub_source_key, is_listing_url, normalize_url,
    recipe_source_url, scrape_from_seeds, scrape_new_recipes, HtmlFetcher, HttpFetcher, ScrapeEvent,
    ScrapeOutcome, ScrapeParams,
};
pub use epub::ingest_epub;
pub use file::FileSource;
pub use manual::read_manual_recipe;
pub use ocr::ImageOcrSource;
pub use quality::is_cookable;
pub use search::{
    duckduckgo_search_url, parse_duckduckgo_results, search_result_urls, search_scrape_recipes,
    unwrap_ddg_redirect,
};
pub use url::UrlSource;

use crate::domain::Recipe;
use anyhow::{bail, Result};
use std::path::Path;

/// Trait for recipe ingestion backends. Produces a normalized [`Recipe`]
/// (ingredient lines may be free text that [`crate::normalize`] will parse).
pub trait RecipeSourceIngest {
    fn ingest(&self, input: &str) -> Result<Recipe>;
    fn name(&self) -> &'static str;
}

/// Dispatch on source kind: `file`, `url`, `image` / `ocr`, `epub`, or auto-detect.
///
/// EPUB books can yield multiple recipes — use [`ingest_epub`] / [`ingest_many`] instead.
pub fn ingest_from(source: &str, input: &str) -> Result<Recipe> {
    let source = source.to_lowercase();
    match source.as_str() {
        "file" | "json" | "toml" | "manual" => FileSource.ingest(input),
        "url" | "web" => UrlSource::default().ingest(input),
        "image" | "ocr" => ImageOcrSource.ingest(input),
        "epub" | "ebook" => bail!("EPUB may contain multiple recipes; use import epub (batch)"),
        "auto" => auto_detect(input),
        other => bail!("unknown source '{other}'; use file, url, image, epub, or auto"),
    }
}

/// Ingest one or many recipes (EPUB batch, or a single recipe for other sources).
pub fn ingest_many(source: &str, input: &str) -> Result<Vec<Recipe>> {
    let source = source.to_lowercase();
    match source.as_str() {
        "epub" | "ebook" => ingest_epub(input),
        "auto" => {
            let path = Path::new(input.trim());
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
            {
                return ingest_epub(input.trim());
            }
            Ok(vec![auto_detect(input)?])
        }
        _ => Ok(vec![ingest_from(&source, input)?]),
    }
}

fn auto_detect(input: &str) -> Result<Recipe> {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return UrlSource::default().ingest(trimmed);
    }
    let path = Path::new(trimmed);
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext.to_lowercase().as_str() {
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" => {
                return ImageOcrSource.ingest(trimmed);
            }
            "epub" => bail!("EPUB may contain multiple recipes; use import epub or import auto"),
            "json" | "toml" | "txt" | "md" => return FileSource.ingest(trimmed),
            _ => {}
        }
    }
    if path.exists() {
        return FileSource.ingest(trimmed);
    }
    bail!("could not auto-detect source for '{input}'; pass file|url|image|epub explicitly")
}
