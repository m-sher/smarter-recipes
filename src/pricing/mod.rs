//! Package sizes and prices for shopping optimization.
//!
//! Provides an offline default catalog so purchase optimization works without
//! network access. Optional store sources implement [`ProductSource`] for live
//! or fixture-backed package data.

mod catalog;
mod density;
mod store;

pub use catalog::{Package, PackageCatalog};
pub use density::{density_g_per_ml, mass_g_to_volume_ml, volume_ml_to_mass_g};
pub use store::{
    enrich_catalog_from_source, FixtureStoreSource, OpenFoodFactsSource, ProductSource,
};
