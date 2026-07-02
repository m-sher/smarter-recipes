//! Meal planning that maximizes ingredient overlap across the schedule.
//!
//! # Algorithm
//!
//! We need to fill `days * meals_per_day` slots from a pool of candidate recipes,
//! ordering them so ingredients purchased for earlier meals are reused later
//! (fewer one-off ingredients → fewer shopping trips and less waste).
//!
//! Pure maximum-overlap scheduling on a multiset of recipes is combinatorial.
//! We use a practical greedy heuristic that works well for household-scale pools
//! (tens to low hundreds of recipes):
//!
//! 1. **Score the pool** — for each recipe, compute its *ingredient key set*
//!    (`IngredientKey` = normalized name + unit kind). Recipes with more keys
//!    that appear in other pool recipes get a higher *global popularity* bonus.
//!
//! 2. **Seed** — pick the recipe with the highest popularity as the first meal
//!    (day 0, meal 0). Ties break by title for determinism.
//!
//! 3. **Greedy extension** — while slots remain, choose the unused candidate that
//!    maximizes:
//!    ```text
//!    score(r) = |keys(r) ∩ coverage| * W_overlap
//!             + |keys(r) ∩ recent_window| * W_recency
//!             + popularity(r) * W_pop
//!             - |keys(r) - coverage| * W_new
//!    ```
//!    where `coverage` is the set of ingredient keys already used by selected
//!    recipes, and `recent_window` is keys from the last `window` meals (default 3)
//!    to prefer reusing *soon* after purchase.
//!
//! 4. **Optional repeats** — if the pool is smaller than the number of slots,
//!    recipes may be reused only after every pool member has been used once in
//!    the current pass (round-robin reuse), unless `allow_repeats` is false, in
//!    which case we stop early with a partial plan.
//!
//! The result is a locally optimal ordering biased toward early consolidation of
//! shopping needs. Exact ILP/CP-SAT solvers would be more optimal for large pools
//! but are heavyweight; this keeps planning offline, fast, and fully testable.
//!
//! Complexity: O(S * P * K) where S = slots, P = pool size, K = avg keys/recipe.

use crate::domain::{IngredientKey, MealPlan, PlannedMeal, Recipe, RecipeId};
use std::collections::{HashMap, HashSet};

const W_OVERLAP: f64 = 3.0;
const W_RECENCY: f64 = 2.0;
const W_POP: f64 = 0.5;
const W_NEW: f64 = 0.25;
const RECENT_WINDOW: usize = 3;

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub days: u32,
    pub meals_per_day: u32,
    /// Allow using the same recipe more than once when the pool is small.
    pub allow_repeats: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            days: 7,
            meals_per_day: 1,
            allow_repeats: true,
        }
    }
}

fn recipe_keys(recipe: &Recipe) -> HashSet<IngredientKey> {
    recipe
        .ingredients
        .iter()
        .map(IngredientKey::from_line)
        .collect()
}

/// Build a meal plan from a candidate pool.
pub fn plan_meals(pool: &[Recipe], opts: &PlanOptions) -> MealPlan {
    let slots = (opts.days * opts.meals_per_day) as usize;
    let plan_id = uuid::Uuid::new_v4().to_string();

    if pool.is_empty() || slots == 0 {
        return MealPlan {
            id: plan_id,
            days: opts.days,
            meals_per_day: opts.meals_per_day,
            meals: vec![],
            rationale: "Empty pool or zero slots; no meals planned.".into(),
        };
    }

    let keys: Vec<HashSet<IngredientKey>> = pool.iter().map(recipe_keys).collect();

    // Global popularity: how many other recipes share each key, summed per recipe.
    let mut key_freq: HashMap<IngredientKey, usize> = HashMap::new();
    for ks in &keys {
        for k in ks {
            *key_freq.entry(k.clone()).or_insert(0) += 1;
        }
    }
    let popularity: Vec<f64> = keys
        .iter()
        .map(|ks| {
            ks.iter()
                .map(|k| (*key_freq.get(k).unwrap_or(&1) - 1) as f64)
                .sum::<f64>()
        })
        .collect();

    // Seed: highest popularity, then title
    let mut seed = 0usize;
    for i in 1..pool.len() {
        let better = popularity[i] > popularity[seed]
            || (popularity[i] == popularity[seed] && pool[i].title < pool[seed].title);
        if better {
            seed = i;
        }
    }

    let mut selected: Vec<usize> = Vec::with_capacity(slots);
    selected.push(seed);
    let mut coverage: HashSet<IngredientKey> = keys[seed].clone();
    let mut used_in_pass: HashSet<usize> = HashSet::new();
    used_in_pass.insert(seed);

    while selected.len() < slots {
        let recent: HashSet<IngredientKey> = selected
            .iter()
            .rev()
            .take(RECENT_WINDOW)
            .flat_map(|&i| keys[i].iter().cloned())
            .collect();

        let mut best: Option<(usize, f64)> = None;
        for (i, ks) in keys.iter().enumerate() {
            if !opts.allow_repeats && selected.contains(&i) {
                continue;
            }
            // Prefer unused recipes in the current pass
            let repeat_penalty = if used_in_pass.contains(&i) {
                if used_in_pass.len() < pool.len() {
                    continue; // still have unused in this pass
                }
                -1.0 // mild penalty when forced to reuse
            } else {
                0.0
            };

            let overlap = ks.intersection(&coverage).count() as f64;
            let recency = ks.intersection(&recent).count() as f64;
            let new_keys = ks.difference(&coverage).count() as f64;
            let score = overlap * W_OVERLAP + recency * W_RECENCY + popularity[i] * W_POP
                - new_keys * W_NEW
                + repeat_penalty;

            let take = match best {
                None => true,
                Some((bi, bs)) => score > bs || (score == bs && pool[i].title < pool[bi].title),
            };
            if take {
                best = Some((i, score));
            }
        }

        let Some((choice, _)) = best else {
            break;
        };
        selected.push(choice);
        coverage.extend(keys[choice].iter().cloned());
        used_in_pass.insert(choice);
        if used_in_pass.len() == pool.len() {
            used_in_pass.clear(); // new pass for repeats
        }
    }

    let meals: Vec<PlannedMeal> = selected
        .iter()
        .enumerate()
        .map(|(idx, &ri)| {
            let day = (idx as u32) / opts.meals_per_day;
            let meal = (idx as u32) % opts.meals_per_day;
            PlannedMeal {
                day,
                meal,
                recipe_id: pool[ri].id.clone(),
                recipe_title: pool[ri].title.clone(),
            }
        })
        .collect();

    let shared = coverage.len();
    let rationale = format!(
        "Greedy overlap planner: {} meal(s) over {} day(s) from a pool of {} recipe(s). \
         Seeded with highest cross-recipe ingredient popularity, then repeatedly chose \
         the candidate maximizing weighted overlap with already-covered ingredients \
         (and a short recency window of {} meal(s)). Plan covers {} distinct ingredient key(s).",
        meals.len(),
        opts.days,
        pool.len(),
        RECENT_WINDOW,
        shared
    );

    MealPlan {
        id: plan_id,
        days: opts.days,
        meals_per_day: opts.meals_per_day,
        meals,
        rationale,
    }
}

/// Ingredient keys introduced by recipes in order (for analysis/tests).
pub fn coverage_prefix(pool: &[Recipe], order: &[RecipeId]) -> Vec<usize> {
    let by_id: HashMap<&str, &Recipe> = pool.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut cov = HashSet::new();
    let mut out = Vec::new();
    for id in order {
        if let Some(r) = by_id.get(id.as_str()) {
            cov.extend(recipe_keys(r));
        }
        out.push(cov.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::normalize_line;

    fn rec(title: &str, ings: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    #[test]
    fn prefers_shared_ingredients_early() {
        // A and B share milk/eggs; C is disjoint spices only
        let a = rec("Pancakes", &["2 cups flour", "1 cup milk", "2 eggs"]);
        let b = rec("French Toast", &["4 slices bread", "1 cup milk", "2 eggs"]);
        let c = rec(
            "Spice Mix",
            &["1 tsp cumin", "1 tsp coriander", "1 tsp turmeric"],
        );
        let pool = vec![a, b, c];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 3,
                meals_per_day: 1,
                allow_repeats: false,
            },
        );
        assert_eq!(plan.meals.len(), 3);
        // First two should be A and B (order may vary but C should not be first if popularity works)
        let titles: Vec<_> = plan.meals.iter().map(|m| m.recipe_title.as_str()).collect();
        // C should be last because it shares nothing with A/B cluster
        assert_eq!(titles[2], "Spice Mix");
        assert!(titles[0] == "Pancakes" || titles[0] == "French Toast");
        assert!(titles[1] == "Pancakes" || titles[1] == "French Toast");
    }

    #[test]
    fn fills_slots_with_repeats() {
        let a = rec("A", &["1 cup rice"]);
        let b = rec("B", &["1 cup rice", "1 onion"]);
        let pool = vec![a, b];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 2,
                meals_per_day: 2,
                allow_repeats: true,
            },
        );
        assert_eq!(plan.meals.len(), 4);
    }

    #[test]
    fn empty_pool() {
        let plan = plan_meals(&[], &PlanOptions::default());
        assert!(plan.meals.is_empty());
    }

    #[test]
    fn deterministic_tiebreak() {
        let a = rec("Alpha", &["1 cup water"]);
        let b = rec("Beta", &["1 cup water"]);
        let pool1 = vec![a.clone(), b.clone()];
        let pool2 = vec![b, a];
        let p1 = plan_meals(
            &pool1,
            &PlanOptions {
                days: 1,
                meals_per_day: 1,
                allow_repeats: false,
            },
        );
        let p2 = plan_meals(
            &pool2,
            &PlanOptions {
                days: 1,
                meals_per_day: 1,
                allow_repeats: false,
            },
        );
        // Same popularity → alphabetical title wins as seed
        assert_eq!(p1.meals[0].recipe_title, p2.meals[0].recipe_title);
        assert_eq!(p1.meals[0].recipe_title, "Alpha");
    }
}
