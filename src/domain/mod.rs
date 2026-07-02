//! Domain types shared across the crate.

mod ingredient;
mod plan;
mod recipe;
mod units;

pub use ingredient::{is_all_descriptors, IngredientKey, IngredientLine, ParsedIngredient};
pub use plan::{MealPlan, PackagePick, PlannedMeal, ShoppingItem, ShoppingList};
pub use recipe::{Recipe, RecipeId, RecipeMeta, RecipeSource};
pub use units::{Unit, UnitKind};
