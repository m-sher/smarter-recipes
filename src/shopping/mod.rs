//! Purchase optimization: choose package sizes covering required amounts.
//!
//! # Algorithm
//!
//! For each ingredient with a required amount `R` (canonical units) and a catalog
//! of packages `(size_i, price_i?)`:
//!
//! 1. Enumerate multisets of packages whose total size `P >= R`, bounding the
//!    search to at most `max_packages` items and sizes that are not wildly larger
//!    than `R` (unless only large sizes exist).
//!
//! 2. Rank feasible combinations by:
//!    - **Primary:** minimum total cost (sum of unit prices). Items without prices
//!      are treated as equal cost 0 so leftover becomes the discriminator.
//!    - **Secondary:** minimum leftover `P - R`.
//!    - **Tertiary:** fewer packages (simpler shopping).
//!
//! 3. Flag leftover when `P - R > epsilon` (default 1% of `R` or absolute 1.0
//!    canonical unit, whichever is larger).
//!
//! Example (from the project plan): 14 oz milk needed, packages 16 oz / 32 oz
//! → prefer 1×16 oz (2 oz leftover) over 1×32 oz. If 32 oz needed and both
//! 2×16 and 1×32 have zero leftover, prefer whichever has lower cost.
//!
//! All logic is pure and online-catalog independent; [`crate::pricing`] supplies
//! the package list (offline defaults or scraped).

mod optimize;

pub use optimize::{optimize_purchase, optimize_shopping_list, OptimizeOptions};

use crate::domain::{MealPlan, ShoppingList};
use crate::pricing::PackageCatalog;
use crate::storage::Store;
use anyhow::Result;

/// Build an optimized shopping list for a stored plan.
pub fn shopping_list_for_plan(
    store: &Store,
    plan: &MealPlan,
    catalog: &PackageCatalog,
) -> Result<ShoppingList> {
    let ids: Vec<_> = plan.meals.iter().map(|m| m.recipe_id.clone()).collect();
    let requirements = store.aggregate_ingredients(&ids)?;
    Ok(optimize_shopping_list(
        &plan.id,
        &requirements,
        catalog,
        &OptimizeOptions::default(),
    ))
}
