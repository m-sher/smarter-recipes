//! Nutrition facts: published per-serving values and computed macro totals.

use serde::{Deserialize, Serialize};

/// Macronutrients as published by a recipe source (schema.org
/// `NutritionInformation`), per serving. Fields are optional because published
/// data is usually partial.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Nutrition {
    pub kcal: Option<f64>,
    pub protein_g: Option<f64>,
    pub fat_g: Option<f64>,
    pub carbs_g: Option<f64>,
}

impl Nutrition {
    pub fn is_empty(&self) -> bool {
        self.kcal.is_none()
            && self.protein_g.is_none()
            && self.fat_g.is_none()
            && self.carbs_g.is_none()
    }
}

/// Complete macro totals used for computed estimates (per 100 g when used as a
/// profile, absolute grams/kcal when used as a running total).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Macros {
    pub kcal: f64,
    pub protein_g: f64,
    pub fat_g: f64,
    pub carbs_g: f64,
}

impl Macros {
    pub fn add_scaled(&mut self, per_100g: &Macros, grams: f64) {
        let f = grams / 100.0;
        self.kcal += per_100g.kcal * f;
        self.protein_g += per_100g.protein_g * f;
        self.fat_g += per_100g.fat_g * f;
        self.carbs_g += per_100g.carbs_g * f;
    }

    pub fn add(&mut self, other: &Macros) {
        self.kcal += other.kcal;
        self.protein_g += other.protein_g;
        self.fat_g += other.fat_g;
        self.carbs_g += other.carbs_g;
    }
}
