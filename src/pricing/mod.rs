//! Package sizes and prices for shopping optimization.
//!
//! Provides an offline default catalog so purchase optimization works without
//! network access. Optional best-effort scraping hooks exist for extension.

mod catalog;

pub use catalog::{Package, PackageCatalog};

/// Try to refresh catalog entries from a store URL (best-effort).
/// Default implementation returns an error explaining that store APIs vary;
/// callers should fall back to the embedded catalog.
pub fn scrape_packages(_store_url: &str, _query: &str) -> anyhow::Result<Vec<Package>> {
    anyhow::bail!(
        "live store scraping is best-effort and store-specific; \
         use the built-in package catalog or extend pricing::scrape_packages \
         for your preferred retailer. Pass --catalog to load a JSON catalog."
    )
}
