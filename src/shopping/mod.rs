//! Purchase optimization: choose package sizes covering required amounts.
//!
//! # Algorithm
//!
//! For each ingredient with a required amount `R` (canonical units) and a catalog
//! of packages `(size_i, price_i?)`:
//!
//! 1. Enumerate multisets of packages whose total size `P >= R`, bounding the
//!    search to at most `max_packages` items.
//!
//! 2. Rank feasible combinations by:
//!    - **Primary:** minimum total cost (unknown if any package lacks a price)
//!    - **Secondary:** minimum leftover `P - R`
//!    - **Tertiary:** fewer packages
//!
//! Dry goods measured by volume in recipes are converted to mass via
//! [`crate::pricing::density`] when a density is known, so packages and leftovers
//! reflect real grocery units (lb/oz bags), not fictitious fl-oz flour.
//!
//! # Per-trip / ordering benefit
//!
//! [`trip_breakdown_for_plan`] walks meals in plan order and reports which
//! ingredient keys are **newly introduced** at each meal (not seen in earlier
//! meals). That makes the planner's ordering observable: front-loading shared
//! ingredients reduces the number of trips that introduce new keys. A "trip" is
//! modeled as one shopping run per day by default (configurable).

mod optimize;

pub use optimize::{optimize_purchase, optimize_shopping_list, OptimizeOptions};

use crate::domain::{IngredientKey, MealPlan, ShoppingList};
use crate::pricing::PackageCatalog;
use crate::storage::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

/// New ingredients introduced at a given meal (for per-trip reporting).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TripStep {
    pub day: u32,
    pub meal: u32,
    pub recipe_title: String,
    /// Ingredient keys first required by this meal (not in any earlier meal).
    pub new_ingredient_keys: Vec<String>,
    pub new_count: usize,
    pub cumulative_unique: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TripBreakdown {
    pub plan_id: String,
    pub steps: Vec<TripStep>,
    /// Unique ingredients if meals were in plan order (cumulative at end).
    pub total_unique_ingredients: usize,
    /// Sum of new keys per day (one "trip" per day): how many first-seen keys per day.
    pub new_keys_per_day: Vec<usize>,
    /// Counterfactual: average new keys per day if meal order were reversed.
    pub reversed_new_keys_per_day: Vec<usize>,
    /// How many fewer first-seen keys appear in the first half of days vs reversed order.
    pub ordering_front_load_advantage: i32,
    pub summary: String,
}

/// Explain how plan ordering affects when ingredients are first needed (proxy for trips).
pub fn trip_breakdown_for_plan(store: &Store, plan: &MealPlan) -> Result<TripBreakdown> {
    let mut steps = Vec::new();
    let mut coverage: HashSet<IngredientKey> = HashSet::new();
    let mut per_day_new: Vec<usize> = vec![0; plan.days as usize];

    for m in &plan.meals {
        let recipe = store
            .get_recipe(m.recipe_id.as_str())?
            .ok_or_else(|| anyhow::anyhow!("recipe {} missing", m.recipe_id))?;
        let mut new_keys = Vec::new();
        for line in &recipe.ingredients {
            let key = IngredientKey::from_line(line);
            if coverage.insert(key.clone()) {
                new_keys.push(format!("{} ({:?})", key.name, key.kind));
            }
        }
        let n = new_keys.len();
        if let Some(slot) = per_day_new.get_mut(m.day as usize) {
            *slot += n;
        }
        steps.push(TripStep {
            day: m.day,
            meal: m.meal,
            recipe_title: m.recipe_title.clone(),
            new_count: n,
            new_ingredient_keys: new_keys,
            cumulative_unique: coverage.len(),
        });
    }

    // Reversed meal order counterfactual
    let mut rev_coverage: HashSet<IngredientKey> = HashSet::new();
    let mut rev_per_day: Vec<usize> = vec![0; plan.days as usize];
    for m in plan.meals.iter().rev() {
        let recipe = store
            .get_recipe(m.recipe_id.as_str())?
            .ok_or_else(|| anyhow::anyhow!("recipe {} missing", m.recipe_id))?;
        let mut n = 0usize;
        for line in &recipe.ingredients {
            let key = IngredientKey::from_line(line);
            if rev_coverage.insert(key) {
                n += 1;
            }
        }
        // Map reversed position back onto days in reverse schedule order
        if let Some(slot) = rev_per_day.get_mut(m.day as usize) {
            *slot += n;
        }
    }

    let half = (plan.days as usize).div_ceil(2).max(1);
    let front: usize = per_day_new.iter().take(half).sum();
    let rev_front: usize = rev_per_day.iter().take(half).sum();
    let advantage = rev_front as i32 - front as i32;

    let summary = format!(
        "Plan order introduces {} unique ingredient key(s). New keys per day (plan order): {:?}. \
         Reversed meal order would introduce {:?} new keys by day (same day labels). \
         First-half-of-days new-key count: plan={front}, reversed={rev_front} \
         (positive advantage {advantage} means plan front-loads shared ingredients better / \
         spreads fewer brand-new keys early than the reverse order).",
        coverage.len(),
        per_day_new,
        rev_per_day,
        advantage = advantage
    );

    Ok(TripBreakdown {
        plan_id: plan.id.clone(),
        steps,
        total_unique_ingredients: coverage.len(),
        new_keys_per_day: per_day_new,
        reversed_new_keys_per_day: rev_per_day,
        ordering_front_load_advantage: advantage,
        summary,
    })
}
