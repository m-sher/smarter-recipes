//! Domain types shared across the crate.

mod ingredient;
mod pantry;
mod plan;
mod recipe;
mod units;

pub use ingredient::{
    is_all_descriptors, normalize_ingredient_name, IngredientKey, IngredientLine, ParsedIngredient,
};
pub use pantry::PantryItem;
pub use plan::{MealPlan, PackagePick, PlannedMeal, ShoppingItem, ShoppingList};
pub use recipe::{Recipe, RecipeId, RecipeMeta, RecipeSource};
pub use units::{Unit, UnitKind};
