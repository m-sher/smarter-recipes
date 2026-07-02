//! Pantry inventory: ingredients already on hand.

use super::ingredient::IngredientKey;
use serde::{Deserialize, Serialize};

/// One stocked ingredient with a quantity in canonical units (g / ml / ea).
///
/// Identity matches recipes and shopping: [`IngredientKey`] is
/// `(normalized_name, UnitKind)`. Quantities use the same bases as the rest of
/// the system (grams, milliliters, each).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PantryItem {
    pub key: IngredientKey,
    /// Amount on hand in canonical units for `key.kind`.
    pub quantity_canonical: f64,
}
