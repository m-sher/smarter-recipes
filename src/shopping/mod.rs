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
//! meals). That makes the planner's ordering observable: shared ingredients
//! appear once as "new" on the earliest meal that needs them, then are covered
//! for later meals. A "trip" is modeled as one shopping day; `new_keys_per_day`
//! sums first-seen keys by plan day.

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
    /// Unique ingredients across the whole plan.
    pub total_unique_ingredients: usize,
    /// First-seen ingredient keys summed by plan day (one shopping trip per day).
    pub new_keys_per_day: Vec<usize>,
    /// Human-readable explanation of what the ordering implies for shopping.
    pub summary: String,
}

/// Explain how plan ordering affects when ingredients are first needed (proxy for trips).
///
/// Walks meals in schedule order and records which ingredient keys appear for the
/// first time at each meal. Later meals that reuse those keys contribute 0 new keys
/// — so a min-union construction order shows large `new_count` early and smaller
/// increments later when ingredients are reused.
pub fn trip_breakdown_for_plan(store: &Store, plan: &MealPlan) -> Result<TripBreakdown> {
    let mut steps = Vec::new();
    let mut coverage: HashSet<IngredientKey> = HashSet::new();
    let mut per_day_new: Vec<usize> = vec![0; plan.days.max(1) as usize];

    for m in &plan.meals {
        let recipe = store
            .get_recipe(m.recipe_id.as_str())?
            .ok_or_else(|| anyhow::anyhow!("recipe {} missing", m.recipe_id))?;
        let mut new_keys = Vec::new();
        for line in &recipe.ingredients {
            let key = IngredientKey::from_line(line);
            if coverage.insert(key.clone()) {
                new_keys.push(key.name.clone());
            }
        }
        let n = new_keys.len();
        let day_idx = m.day as usize;
        if day_idx < per_day_new.len() {
            per_day_new[day_idx] += n;
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

    let total = coverage.len();
    let mid = steps.len() / 2;
    let early_new: usize = steps.iter().take(mid).map(|s| s.new_count).sum();
    let late_new: usize = steps.iter().skip(mid).map(|s| s.new_count).sum();
    let summary = if steps.is_empty() {
        "No meals in plan; nothing to analyze.".into()
    } else {
        format!(
            "Walked {} meal(s) in plan order; {} distinct ingredient key(s) in total. \
             New keys introduced per day (trip proxy): {:?}. \
             First half of meals introduced {} new key(s); second half introduced {} \
             (a well-ordered min-union plan typically shows more new keys early, then reuse). \
             Each step lists exactly which keys are first required at that meal — later meals \
             that only reuse earlier ingredients show new_count = 0.",
            steps.len(),
            total,
            per_day_new,
            early_new,
            late_new
        )
    };

    Ok(TripBreakdown {
        plan_id: plan.id.clone(),
        steps,
        total_unique_ingredients: total,
        new_keys_per_day: per_day_new,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Recipe;
    use crate::normalize::normalize_line;
    use tempfile::TempDir;

    fn rec(title: &str, ings: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    #[test]
    fn trip_steps_mark_reuse_as_zero_new() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let a = rec("A", &["1 cup milk", "2 eggs"]);
        let b = rec("B", &["1 cup milk", "1 cup flour"]);
        store.save_recipe(&a).unwrap();
        store.save_recipe(&b).unwrap();
        let plan = MealPlan {
            id: "p1".into(),
            days: 2,
            meals_per_day: 1,
            meals: vec![
                crate::domain::PlannedMeal {
                    day: 0,
                    meal: 0,
                    recipe_id: a.id.clone(),
                    recipe_title: a.title.clone(),
                },
                crate::domain::PlannedMeal {
                    day: 1,
                    meal: 0,
                    recipe_id: b.id.clone(),
                    recipe_title: b.title.clone(),
                },
            ],
            rationale: "test".into(),
        };
        let t = trip_breakdown_for_plan(&store, &plan).unwrap();
        assert_eq!(t.steps[0].new_count, 2); // milk, eggs
        assert_eq!(t.steps[1].new_count, 1); // flour only; milk reused
        assert_eq!(t.total_unique_ingredients, 3);
        assert!(!t.summary.contains("positive advantage"));
        assert!(!t.summary.contains("reversed"));
        // Ingredient keys read as plain names, not "name (Kind)".
        assert!(t.steps[0]
            .new_ingredient_keys
            .iter()
            .all(|k| !k.contains('(')));
        assert!(t.steps[0].new_ingredient_keys.contains(&"milk".to_string()));
    }
}
