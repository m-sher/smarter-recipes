//! Pluggable recipe ingestion sources.
//!
//! Add a new source by implementing [`RecipeSource`] and registering it in
//! [`ingest_from`] / the CLI `import` command.

mod crawl;
mod file;
mod manual;
mod ocr;
mod quality;
mod search;
mod url;

pub use crawl::{
    discover_recipe_links, discover_scoped_links, is_listing_url, normalize_url, recipe_source_url,
    scrape_from_seeds, scrape_new_recipes, HttpFetcher, ScrapeEvent, ScrapeOutcome, ScrapeParams,
};
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

/// Trait for recipe ingestion backends. Keep implementations focused on I/O;
/// always produce a normalized [`Recipe`] (ingredient lines may still be free text
/// that [`crate::normalize`] will parse).
pub trait RecipeSourceIngest {
    fn ingest(&self, input: &str) -> Result<Recipe>;
    fn name(&self) -> &'static str;
}

/// Dispatch on source kind: `file`, `url`, `image` / `ocr`, or auto-detect.
pub fn ingest_from(source: &str, input: &str) -> Result<Recipe> {
    let source = source.to_lowercase();
    match source.as_str() {
        "file" | "json" | "toml" | "manual" => FileSource.ingest(input),
        "url" | "web" => UrlSource::default().ingest(input),
        "image" | "ocr" => ImageOcrSource.ingest(input),
        "auto" => auto_detect(input),
        other => bail!("unknown source '{other}'; use file, url, image, or auto"),
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
            "json" | "toml" | "txt" | "md" => return FileSource.ingest(trimmed),
            _ => {}
        }
    }
    if path.exists() {
        return FileSource.ingest(trimmed);
    }
    bail!("could not auto-detect source for '{input}'; pass file|url|image explicitly")
}
