//! Smarter Recipes — meal planning CLI library.
//!
//! Core modules are independent of network/OCR so they can be tested offline:
//! - [`domain`] — shared types (recipes, ingredients, plans, units)
//! - [`normalize`] — free-text ingredient parsing and unit normalization
//! - [`planning`] — meal plans maximizing ingredient overlap
//! - [`shopping`] — package-size purchase optimization
//!
//! I/O modules:
//! - [`ingest`] — pluggable recipe sources (file, URL, image/OCR)
//! - [`storage`] — SQLite persistence with ingredient dedup
//! - [`pricing`] — package catalog (offline defaults + optional scrape)

pub mod cli;
pub mod domain;
pub mod ingest;
pub mod normalize;
pub mod planning;
pub mod pricing;
pub mod shopping;
pub mod storage;

pub use domain::{IngredientLine, MealPlan, Recipe, UnitKind};
