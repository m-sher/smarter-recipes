//! Meal planning that minimizes the total number of distinct ingredients.
//!
//! # Objective
//!
//! Fill up to `days * meals_per_day` slots from a candidate pool **without
//! repeating any recipe**, choosing a set whose combined ingredient vocabulary
//! is as small as possible. Fewer distinct ingredients → shorter shopping lists
//! and better package utilization.
//!
//! Ingredient identity uses [`IngredientKey`] (normalized name + unit kind),
//! matching aggregation and shopping.
//!
//! # Algorithm
//!
//! Exact minimum-union subset selection is combinatorial. For household-scale
//! pools we use a **multi-start greedy** construction:
//!
//! 1. **Target size** — `S = min(slots, pool.len())`. If the pool is smaller
//!    than the number of slots, the plan is partial (never reuse a recipe).
//!
//! 2. **Greedy growth from a seed** — start with one recipe as the first meal.
//!    While fewer than `S` recipes are selected, append the unused candidate
//!    that minimizes the number of **new** ingredient keys (keys not already in
//!    the running union). Ties break by:
//!    - smaller `|keys(candidate)|` (prefer compact recipes),
//!    - then lexicographically smaller title (determinism).
//!
//! 3. **Multi-start** — run the greedy growth once for **every** pool member as
//!    seed. Keep the schedule with the smallest final `|union|`. If two
//!    schedules tie on union size, prefer the one whose sequence of titles is
//!    lexicographically smaller (stable, deterministic).
//!
//! Construction order is the plan order: ingredients tend to appear when first
//! needed, which keeps [`crate::shopping::trip_breakdown_for_plan`] meaningful.
//!
//! Complexity: O(P² · S · K) where P = pool size, S = slots, K = avg keys/recipe
//! — fine for tens to low hundreds of recipes.

use crate::domain::{IngredientKey, MealPlan, PlannedMeal, Recipe, RecipeId};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub days: u32,
    pub meals_per_day: u32,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            days: 7,
            meals_per_day: 1,
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

/// Union size of ingredient keys for recipes at the given pool indices.
fn union_size(keys: &[HashSet<IngredientKey>], indices: &[usize]) -> usize {
    let mut u: HashSet<&IngredientKey> = HashSet::new();
    for &i in indices {
        u.extend(keys[i].iter());
    }
    u.len()
}

/// Greedy growth from `seed`: always add the unused recipe that introduces the
/// fewest new keys. Returns selected pool indices in plan order.
fn greedy_from_seed(
    pool: &[Recipe],
    keys: &[HashSet<IngredientKey>],
    seed: usize,
    target: usize,
) -> Vec<usize> {
    let mut selected = Vec::with_capacity(target);
    selected.push(seed);
    let mut coverage = keys[seed].clone();
    let mut used = HashSet::new();
    used.insert(seed);

    while selected.len() < target {
        let mut best: Option<(usize, usize, usize)> = None; // (idx, new_keys, key_count)
        for (i, ks) in keys.iter().enumerate() {
            if used.contains(&i) {
                continue;
            }
            let new_keys = ks.difference(&coverage).count();
            let key_count = ks.len();
            let take = match best {
                None => true,
                Some((bi, bn, bk)) => {
                    new_keys < bn
                        || (new_keys == bn && key_count < bk)
                        || (new_keys == bn && key_count == bk && pool[i].title < pool[bi].title)
                }
            };
            if take {
                best = Some((i, new_keys, key_count));
            }
        }
        let Some((choice, _, _)) = best else {
            break;
        };
        selected.push(choice);
        coverage.extend(keys[choice].iter().cloned());
        used.insert(choice);
    }
    selected
}

/// Compare two schedules for multi-start selection: smaller union wins; ties by
/// lexicographic title sequence.
fn better_schedule(
    pool: &[Recipe],
    keys: &[HashSet<IngredientKey>],
    a: &[usize],
    b: &[usize],
) -> bool {
    let ua = union_size(keys, a);
    let ub = union_size(keys, b);
    if ua != ub {
        return ua < ub;
    }
    for (&ia, &ib) in a.iter().zip(b.iter()) {
        match pool[ia].title.cmp(&pool[ib].title) {
            std::cmp::Ordering::Less => return true,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    a.len() < b.len()
}

/// Build a meal plan from a candidate pool (no recipe repeats).
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
    let target = slots.min(pool.len());

    let mut best: Option<Vec<usize>> = None;
    for seed in 0..pool.len() {
        let candidate = greedy_from_seed(pool, &keys, seed, target);
        let take = match &best {
            None => true,
            Some(b) => better_schedule(pool, &keys, &candidate, b),
        };
        if take {
            best = Some(candidate);
        }
    }

    let selected = best.unwrap_or_default();
    let total_unique = union_size(&keys, &selected);

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

    let partial_note = if meals.len() < slots {
        format!(
            " Pool has only {} recipe(s); requested {} slot(s), so the plan is partial (repeats are never used).",
            pool.len(),
            slots
        )
    } else {
        String::new()
    };

    let rationale = format!(
        "Min-union planner: {} meal(s) over {} day(s) from a pool of {} recipe(s). \
         Multi-start greedy selection minimizes distinct ingredient keys \
         (no recipe repeats). Plan uses {} distinct ingredient key(s).{}",
        meals.len(),
        opts.days,
        pool.len(),
        total_unique,
        partial_note
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

/// Distinct ingredient key count for a plan's recipes (for tests/analysis).
pub fn plan_union_size(pool: &[Recipe], plan: &MealPlan) -> usize {
    let by_id: HashMap<&str, &Recipe> = pool.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut cov = HashSet::new();
    for m in &plan.meals {
        if let Some(r) = by_id.get(m.recipe_id.as_str()) {
            cov.extend(recipe_keys(r));
        }
    }
    cov.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::normalize_line;
    use std::collections::HashSet as StdHashSet;

    fn rec(title: &str, ings: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    fn titles(plan: &MealPlan) -> Vec<&str> {
        plan.meals.iter().map(|m| m.recipe_title.as_str()).collect()
    }

    fn unique_recipe_ids(plan: &MealPlan) -> usize {
        plan.meals
            .iter()
            .map(|m| m.recipe_id.as_str())
            .collect::<StdHashSet<_>>()
            .len()
    }

    #[test]
    fn prefers_compact_shared_cluster_over_disjoint() {
        // A and B share milk/eggs (small union); C is disjoint spices.
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
                days: 2,
                meals_per_day: 1,
            },
        );
        assert_eq!(plan.meals.len(), 2);
        let t = titles(&plan);
        assert!(t.contains(&"Pancakes"));
        assert!(t.contains(&"French Toast"));
        assert!(!t.contains(&"Spice Mix"));
        // Union of A+B is smaller than any pair involving C.
        assert_eq!(plan_union_size(&pool, &plan), 4); // flour, milk, eggs, bread
    }

    #[test]
    fn never_repeats_recipes() {
        let a = rec("A", &["1 cup rice"]);
        let b = rec("B", &["1 cup rice", "1 onion"]);
        let pool = vec![a, b];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 2,
                meals_per_day: 2, // 4 slots, only 2 recipes
            },
        );
        assert_eq!(plan.meals.len(), 2);
        assert_eq!(unique_recipe_ids(&plan), 2);
        assert!(plan.rationale.contains("partial"));
    }

    #[test]
    fn empty_pool() {
        let plan = plan_meals(&[], &PlanOptions::default());
        assert!(plan.meals.is_empty());
    }

    #[test]
    fn zero_slots() {
        let a = rec("A", &["1 cup water"]);
        let plan = plan_meals(
            &[a],
            &PlanOptions {
                days: 0,
                meals_per_day: 1,
            },
        );
        assert!(plan.meals.is_empty());
    }

    #[test]
    fn deterministic_tiebreak() {
        let a = rec("Alpha", &["1 cup water"]);
        let b = rec("Beta", &["1 cup water"]);
        let pool1 = vec![a.clone(), b.clone()];
        let pool2 = vec![b, a];
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
        };
        let p1 = plan_meals(&pool1, &opts);
        let p2 = plan_meals(&pool2, &opts);
        assert_eq!(p1.meals[0].recipe_title, p2.meals[0].recipe_title);
        assert_eq!(p1.meals[0].recipe_title, "Alpha");
    }

    #[test]
    fn selects_all_when_slots_equal_pool() {
        let pool = vec![
            rec("A", &["1 egg"]),
            rec("B", &["1 egg", "1 milk"]),
            rec("C", &["1 milk"]),
        ];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 3,
                meals_per_day: 1,
            },
        );
        assert_eq!(plan.meals.len(), 3);
        assert_eq!(unique_recipe_ids(&plan), 3);
        assert_eq!(plan_union_size(&pool, &plan), 2);
    }

    #[test]
    fn multi_start_avoids_bad_singleton_seed() {
        // Tiny solo "salt" pairs poorly with the egg cluster. The tight pair
        // Omelette+Scramble shares eggs+butter only (union 2), which beats
        // salt+omelette (union 3) and any exotic pairing.
        let salt = rec("Just Salt", &["1 tsp salt"]);
        let a = rec("Omelette", &["2 eggs", "1 tbsp butter"]);
        let b = rec("Scramble", &["3 eggs", "1 tbsp butter"]);
        let exotic = rec(
            "Exotic Solo",
            &["1 cup dragonfruit", "1 tsp saffron", "1 leaf gold"],
        );
        let pool = vec![salt, a, b, exotic];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
            },
        );
        assert_eq!(plan.meals.len(), 2);
        let t = titles(&plan);
        assert!(t.contains(&"Omelette"));
        assert!(t.contains(&"Scramble"));
        assert!(!t.contains(&"Just Salt"));
        assert!(!t.contains(&"Exotic Solo"));
        assert_eq!(plan_union_size(&pool, &plan), 2);
    }

    #[test]
    fn no_duplicate_ids_in_full_plan() {
        let pool: Vec<_> = (0..5)
            .map(|i| rec(&format!("R{i}"), &["1 cup flour", "1 egg"]))
            .collect();
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 5,
                meals_per_day: 1,
            },
        );
        assert_eq!(plan.meals.len(), 5);
        assert_eq!(unique_recipe_ids(&plan), 5);
    }

    #[test]
    fn coverage_prefix_tracks_growth() {
        let a = rec("A", &["1 cup milk"]);
        let b = rec("B", &["1 cup milk", "1 egg"]);
        let pool = vec![a.clone(), b.clone()];
        let order = vec![a.id.clone(), b.id.clone()];
        let prefix = coverage_prefix(&pool, &order);
        assert_eq!(prefix, vec![1, 2]);
    }

    #[test]
    fn pool_order_does_not_change_selected_set_for_ties() {
        let a = rec("Pancakes", &["2 cups flour", "1 cup milk", "2 eggs"]);
        let b = rec("French Toast", &["4 slices bread", "1 cup milk", "2 eggs"]);
        let c = rec("Spice Mix", &["1 tsp cumin", "1 tsp coriander"]);
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 1,
        };
        let p1 = plan_meals(&[a.clone(), b.clone(), c.clone()], &opts);
        let p2 = plan_meals(&[c, b, a], &opts);
        let mut s1: Vec<_> = titles(&p1);
        let mut s2: Vec<_> = titles(&p2);
        s1.sort();
        s2.sort();
        assert_eq!(s1, s2);
    }
}
