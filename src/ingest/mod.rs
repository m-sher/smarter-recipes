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
    discover_recipe_links, discover_scoped_links, epub_source_key, epub_source_key_for_resolved,
    is_listing_url, normalize_url, recipe_source_url, resolve_epub_path, scrape_from_seeds,
    scrape_new_recipes, HtmlFetcher, HttpFetcher, ScrapeEvent, ScrapeOutcome, ScrapeParams,
};
pub use epub::{ingest_epub, EpubIngestOutcome};
pub use file::FileSource;
pub use manual::read_manual_recipe;
pub use ocr::ImageOcrSource;
pub use quality::{is_cookable, text_has_amount};
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

/// Outcome of [`ingest_many`]: recipes to save plus batch-level skip metadata.
#[derive(Debug, Clone, Default)]
pub struct IngestBatch {
    pub recipes: Vec<Recipe>,
    /// Titles skipped under EPUB never-guess ambiguous-structure policy.
    pub skipped_ambiguous: Vec<String>,
}

impl IngestBatch {
    pub fn recipes_only(recipes: Vec<Recipe>) -> Self {
        Self {
            recipes,
            skipped_ambiguous: Vec::new(),
        }
    }
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
pub fn ingest_many(source: &str, input: &str) -> Result<IngestBatch> {
    let source = source.to_lowercase();
    match source.as_str() {
        "epub" | "ebook" => {
            let o = ingest_epub(input)?;
            Ok(IngestBatch {
                recipes: o.recipes,
                skipped_ambiguous: o.skipped_ambiguous,
            })
        }
        "auto" => {
            let path = Path::new(input.trim());
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
            {
                let o = ingest_epub(input.trim())?;
                return Ok(IngestBatch {
                    recipes: o.recipes,
                    skipped_ambiguous: o.skipped_ambiguous,
                });
            }
            Ok(IngestBatch::recipes_only(vec![auto_detect(input)?]))
        }
        _ => Ok(IngestBatch::recipes_only(vec![ingest_from(
            &source, input,
        )?])),
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
