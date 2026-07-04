//! Smarter Recipes — meal planning CLI library.
//!
//! Core modules are independent of network/OCR so they can be tested offline:
//! - [`domain`] — shared types (recipes, ingredients, plans, pantry, units)
//! - [`normalize`] — free-text ingredient parsing and unit normalization
//! - [`planning`] — meal plans minimizing distinct ingredients (no repeats),
//!   considering on-hand pantry stock
//! - [`shopping`] — package-size purchase optimization, net of pantry
//!
//! I/O modules:
//! - [`ingest`] — pluggable recipe sources (file, URL, image/OCR)
//! - [`storage`] — SQLite persistence with ingredient dedup and pantry stock
//! - [`pricing`] — package catalog (offline defaults + optional scrape)

pub mod cli;
pub mod domain;
pub mod dotenv;
pub mod ingest;
pub mod net;
pub mod normalize;
pub mod nutrition;
pub mod planning;
pub mod pricing;
pub mod shopping;
pub mod storage;
pub mod text;

pub use domain::{IngredientLine, MealPlan, Recipe, UnitKind};
