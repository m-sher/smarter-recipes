//! Meal plans and shopping lists.

use super::ingredient::IngredientKey;
use super::recipe::RecipeId;
use super::units::UnitKind;
use serde::{Deserialize, Serialize};

/// One meal slot in a plan (day index, meal index within day, recipe).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedMeal {
    /// 0-based day index.
    pub day: u32,
    /// 0-based meal index within the day.
    pub meal: u32,
    pub recipe_id: RecipeId,
    pub recipe_title: String,
}

/// Ordered selection of recipes across days.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MealPlan {
    pub id: String,
    pub days: u32,
    pub meals_per_day: u32,
    pub meals: Vec<PlannedMeal>,
    /// Human-readable explanation of why this ordering was chosen.
    pub rationale: String,
}

/// One line on the optimized shopping list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShoppingItem {
    pub ingredient: IngredientKey,
    /// Total required amount in canonical units for `ingredient.kind`.
    pub required_canonical: f64,
    /// Display unit name for the required amount (e.g. "g", "ml", "ea").
    pub required_unit_label: String,
    /// Packages recommended: (package size in same canonical units, count, unit price cents, package label).
    pub packages: Vec<PackagePick>,
    /// Total purchased amount in canonical units.
    pub purchased_canonical: f64,
    /// Leftover amount (purchased - required), canonical units.
    pub leftover_canonical: f64,
    /// Total cost in cents (None if prices unknown).
    pub total_cost_cents: Option<u64>,
    /// True when leftover is non-zero and notable.
    pub leftover_flagged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackagePick {
    pub label: String,
    /// Size in canonical units matching the ingredient's UnitKind.
    pub size_canonical: f64,
    pub count: u32,
    /// Unit price in cents for one package, if known.
    pub unit_price_cents: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShoppingList {
    pub plan_id: String,
    pub items: Vec<ShoppingItem>,
    pub total_cost_cents: Option<u64>,
}

impl ShoppingList {
    pub fn kind_label(kind: UnitKind) -> &'static str {
        match kind {
            UnitKind::Mass => "g",
            UnitKind::Volume => "ml",
            UnitKind::Count => "ea",
            UnitKind::Other => "unit",
        }
    }
}
