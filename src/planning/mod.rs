//! Meal planning that minimizes the total number of distinct ingredients.
//!
//! # Objective
//!
//! Fill up to `days * meals_per_day` slots from a candidate pool **without
//! repeating any recipe** (by [`RecipeId`] **or** normalized title), choosing a
//! set whose combined ingredient vocabulary is as small as possible. Fewer
//! distinct ingredients → shorter shopping lists and better package utilization.
//!
//! Ingredient identity uses [`IngredientKey`] (normalized name + unit kind),
//! matching aggregation and shopping.
//!
//! Keys already present in the optional **pantry** (on-hand stock) do not count
//! as "new" when scoring candidates, and the reported plan cost is the size of
//! the union **minus** pantry keys (ingredients you still need to source).
//!
//! Optional [`NutritionBounds`] steer selection using precomputed whole-recipe
//! estimated [`Macros`]: prefer schedules that satisfy per-meal, per-day, and
//! plan-total min/max ranges; if none are feasible, return the least total
//! violation (then min-union) and list violations in the rationale.
//!
//! # Algorithm
//!
//! Exact minimum-union subset selection is combinatorial. For household-scale
//! pools we use a **multi-start greedy** construction:
//!
//! 1. **Normalize the pool** — (a) keep the first occurrence of each
//!    `recipe_id`, (b) drop recipes with **no** ingredient keys when any
//!    non-empty recipe exists (failed/stub ingests must not crowd out real
//!    meals; if every recipe is empty they are kept so the planner can still
//!    fill slots), then (c) among survivors keep the first of each non-empty
//!    normalized title key (duplicate titles cannot be scheduled twice even
//!    with different ids). Empty filtering runs **before** title collapse so an
//!    empty stub cannot claim a title and block a fuller same-title recipe.
//!
//! 2. **Target size** — `S = min(slots, unique_pool.len())`. If the pool is
//!    smaller than the number of slots, the plan is partial (never reuse a
//!    recipe).
//!
//! 3. **Greedy growth from a seed** — start with one recipe as the first meal.
//!    The running coverage is seeded with pantry keys. While fewer than `S`
//!    recipes are selected, append the unused candidate that minimizes the
//!    number of **new** ingredient keys (keys not already in the running
//!    coverage). Ties break by:
//!    - smaller `|keys(candidate)|` (prefer compact recipes),
//!    - then lexicographically smaller title,
//!    - then lexicographically smaller `recipe_id` (full pool-order independence).
//!
//! 4. **Multi-start** — run the greedy growth once for **every** pool member as
//!    seed. Keep the schedule with the smallest final **net** union size
//!    (`|union − pantry|`). If two schedules tie on that size, prefer the one
//!    whose sequence of `(title, recipe_id)` pairs is lexicographically smaller.
//!    Equal schedules keep the incumbent. When `S == pool.len()`, multi-start
//!    is skipped and a single greedy order is built from the lex-smallest seed.
//!
//! Construction order is the plan order: ingredients tend to appear when first
//! needed, which keeps [`crate::shopping::trip_breakdown_for_plan`] meaningful.
//!
//! Complexity: O(P² · S · K) where P = pool size, S = slots, K = avg keys/recipe
//! — fine for tens to low hundreds of recipes.

mod nutrition_bounds;

pub use nutrition_bounds::{
    evaluate_macros, evaluate_schedule, exceeds_max, load_nutrition_bounds, min_deficit,
    violates_per_meal, violation_magnitude, BoundScope, BoundViolation, CliPerDayNutrition,
    MacroBounds, MacroRange, NutrientKind, NutritionBounds, ViolationKind,
};

use crate::domain::{
    normalize_title_key, IngredientKey, Macros, MealPlan, PlannedMeal, Recipe, RecipeId,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub days: u32,
    pub meals_per_day: u32,
    /// Ingredient keys already on hand; excluded from "new" cost and net union.
    pub pantry: HashSet<IngredientKey>,
    /// Optional macro min/max constraints. Empty keeps legacy min-union behavior.
    pub nutrition: NutritionBounds,
    /// Precomputed whole-recipe estimated macros (missing ids treat as zero).
    pub recipe_macros: HashMap<RecipeId, Macros>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            days: 7,
            meals_per_day: 1,
            pantry: HashSet::new(),
            nutrition: NutritionBounds::default(),
            recipe_macros: HashMap::new(),
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

/// Deduplicate by `recipe_id`, drop empty-ingredient recipes when any non-empty
/// recipe exists, then collapse by normalized title key (first wins among
/// survivors). Empty filtering must run before title collapse so an empty stub
/// cannot claim a title and block a fuller same-title recipe. Returns recipes
/// paired with precomputed key sets so `recipe_keys` runs once per id-unique
/// recipe.
fn normalize_pool(pool: &[Recipe]) -> (Vec<&Recipe>, Vec<HashSet<IngredientKey>>) {
    // Phase 1: id-dedupe and precompute keys.
    let mut seen_ids = HashSet::new();
    let mut recipes: Vec<&Recipe> = Vec::new();
    let mut keys: Vec<HashSet<IngredientKey>> = Vec::new();
    for r in pool {
        if !seen_ids.insert(r.id.as_str()) {
            continue;
        }
        recipes.push(r);
        keys.push(recipe_keys(r));
    }

    // Phase 2: drop empties when any non-empty candidate remains.
    let any_nonempty = keys.iter().any(|ks| !ks.is_empty());
    if any_nonempty {
        let mut i = 0;
        while i < recipes.len() {
            if keys[i].is_empty() {
                recipes.remove(i);
                keys.remove(i);
            } else {
                i += 1;
            }
        }
    }

    // Phase 3: title-key first-wins among survivors only.
    let mut seen_titles = HashSet::new();
    let mut out_recipes: Vec<&Recipe> = Vec::new();
    let mut out_keys: Vec<HashSet<IngredientKey>> = Vec::new();
    for (r, k) in recipes.into_iter().zip(keys) {
        let title_key = normalize_title_key(&r.title);
        if !title_key.is_empty() && !seen_titles.insert(title_key) {
            continue;
        }
        out_recipes.push(r);
        out_keys.push(k);
    }
    (out_recipes, out_keys)
}

/// Full union size of ingredient keys for recipes at the given pool indices.
fn union_size(keys: &[HashSet<IngredientKey>], indices: &[usize]) -> usize {
    let mut u: HashSet<&IngredientKey> = HashSet::new();
    for &i in indices {
        u.extend(keys[i].iter());
    }
    u.len()
}

/// Keys in the selected recipes that are **not** already covered by the pantry
/// (i.e. still need to be sourced).
fn net_union_size(
    keys: &[HashSet<IngredientKey>],
    indices: &[usize],
    pantry: &HashSet<IngredientKey>,
) -> usize {
    let mut u: HashSet<&IngredientKey> = HashSet::new();
    for &i in indices {
        for k in &keys[i] {
            if !pantry.contains(k) {
                u.insert(k);
            }
        }
    }
    u.len()
}

fn macros_for(opts: &PlanOptions, id: &RecipeId) -> Macros {
    opts.recipe_macros.get(id).copied().unwrap_or_default()
}

fn align_macros(pool: &[&Recipe], opts: &PlanOptions) -> Vec<Macros> {
    pool.iter().map(|r| macros_for(opts, &r.id)).collect()
}

fn deficit_key(bounds: &MacroBounds, day_macros: &Macros, add: &Macros) -> u64 {
    let mut trial = *day_macros;
    trial.add(add);
    (min_deficit(bounds, &trial) * 1000.0).round() as u64
}

fn candidate_sort_key<'a>(
    pool: &[&'a Recipe],
    keys: &[HashSet<IngredientKey>],
    coverage: &HashSet<IngredientKey>,
    macros: &[Macros],
    bounds: &NutritionBounds,
    day_macros: &Macros,
    i: usize,
) -> (usize, usize, u64, &'a str, &'a str) {
    let new_keys = keys[i].difference(coverage).count();
    let deficit = if bounds.is_empty() {
        0
    } else {
        deficit_key(&bounds.per_day, day_macros, &macros[i])
    };
    (
        new_keys,
        keys[i].len(),
        deficit,
        pool[i].title.as_str(),
        pool[i].id.as_str(),
    )
}

fn nutrition_allows(bounds: &NutritionBounds, macros: &Macros, day_macros: &Macros) -> bool {
    if !bounds.per_meal.is_empty() && violates_per_meal(&bounds.per_meal, macros) {
        return false;
    }
    if !bounds.per_day.is_empty() && exceeds_max(&bounds.per_day, day_macros, macros) {
        return false;
    }
    true
}

struct GreedyInput<'a> {
    pool: &'a [&'a Recipe],
    keys: &'a [HashSet<IngredientKey>],
    macros: &'a [Macros],
    pantry: &'a HashSet<IngredientKey>,
    bounds: &'a NutritionBounds,
    meals_per_day: u32,
}

/// Greedy growth from `seed`: always add the unused recipe that introduces the
/// fewest new keys (relative to pantry + already selected). Returns selected
/// pool indices in plan order.
fn greedy_from_seed(input: &GreedyInput<'_>, seed: usize, target: usize) -> Vec<usize> {
    let GreedyInput {
        pool,
        keys,
        macros,
        pantry,
        bounds,
        meals_per_day,
    } = input;
    let mpd = (*meals_per_day).max(1);
    let mut selected = Vec::with_capacity(target);
    selected.push(seed);
    let mut coverage = (**pantry).clone();
    coverage.extend(keys[seed].iter().cloned());
    let mut used_ids: HashSet<&str> = HashSet::new();
    used_ids.insert(pool[seed].id.as_str());
    let mut day_macros = macros[seed];
    let mut cur_day = 0u32;

    while selected.len() < target {
        let slot = selected.len() as u32;
        let day = slot / mpd;
        if day != cur_day {
            cur_day = day;
            day_macros = Macros::default();
        }

        let mut best: Option<usize> = None;
        let mut best_key = None;
        let mut best_relaxed: Option<usize> = None;
        let mut best_relaxed_key = None;
        for i in 0..pool.len() {
            if used_ids.contains(pool[i].id.as_str()) {
                continue;
            }
            let key = candidate_sort_key(pool, keys, &coverage, macros, bounds, &day_macros, i);
            let allowed = bounds.is_empty() || nutrition_allows(bounds, &macros[i], &day_macros);
            if allowed {
                if best_key.as_ref().is_none_or(|bk| key < *bk) {
                    best = Some(i);
                    best_key = Some(key);
                }
            } else if best_relaxed_key.as_ref().is_none_or(|bk| key < *bk) {
                best_relaxed = Some(i);
                best_relaxed_key = Some(key);
            }
        }
        let Some(choice) = best.or(best_relaxed) else {
            break;
        };
        selected.push(choice);
        coverage.extend(keys[choice].iter().cloned());
        used_ids.insert(pool[choice].id.as_str());
        day_macros.add(&macros[choice]);
    }
    selected
}

/// True if `a` beats incumbent `b` on lex `(title, id)` sequence only.
fn lex_better_schedule(pool: &[&Recipe], a: &[usize], b: &[usize]) -> bool {
    for (&ia, &ib) in a.iter().zip(b.iter()) {
        match (
            pool[ia].title.as_str().cmp(pool[ib].title.as_str()),
            pool[ia].id.as_str().cmp(pool[ib].id.as_str()),
        ) {
            (std::cmp::Ordering::Less, _) => return true,
            (std::cmp::Ordering::Greater, _) => return false,
            (std::cmp::Ordering::Equal, std::cmp::Ordering::Less) => return true,
            (std::cmp::Ordering::Equal, std::cmp::Ordering::Greater) => return false,
            (std::cmp::Ordering::Equal, std::cmp::Ordering::Equal) => {}
        }
    }
    false
}

/// True if `a` beats incumbent `b`: smaller union wins; ties by lex
/// `(title, id)` sequence. Equal schedules keep the incumbent (`false`).
fn better_schedule(pool: &[&Recipe], a: &[usize], ua: usize, b: &[usize], ub: usize) -> bool {
    if ua != ub {
        return ua < ub;
    }
    lex_better_schedule(pool, a, b)
}

struct ScoredSchedule {
    indices: Vec<usize>,
    net_union: usize,
    magnitude: f64,
    violations: Vec<BoundViolation>,
}

fn score_schedule(
    _pool: &[&Recipe],
    keys: &[HashSet<IngredientKey>],
    macros: &[Macros],
    indices: Vec<usize>,
    pantry: &HashSet<IngredientKey>,
    bounds: &NutritionBounds,
    meals_per_day: u32,
) -> ScoredSchedule {
    let net_union = net_union_size(keys, &indices, pantry);
    let violations = if bounds.is_empty() {
        Vec::new()
    } else {
        schedule_violations(macros, &indices, bounds, meals_per_day)
    };
    let magnitude = violation_magnitude(&violations);
    ScoredSchedule {
        indices,
        net_union,
        magnitude,
        violations,
    }
}

fn schedule_violations(
    macros: &[Macros],
    indices: &[usize],
    bounds: &NutritionBounds,
    meals_per_day: u32,
) -> Vec<BoundViolation> {
    let mpd = meals_per_day.max(1);
    let mut per_day: Vec<(u32, Macros)> = Vec::new();
    let mut per_meal: Vec<(u32, u32, Macros)> = Vec::new();
    let mut plan_total = Macros::default();
    for (slot, &ri) in indices.iter().enumerate() {
        let day = slot as u32 / mpd;
        let meal = slot as u32 % mpd;
        let m = macros[ri];
        per_meal.push((day, meal, m));
        plan_total.add(&m);
        match per_day.last_mut() {
            Some((d, acc)) if *d == day => acc.add(&m),
            _ => per_day.push((day, m)),
        }
    }
    evaluate_schedule(bounds, &per_day, &per_meal, &plan_total)
}

fn better_scored(pool: &[&Recipe], a: &ScoredSchedule, b: &ScoredSchedule) -> bool {
    let a_ok = a.magnitude == 0.0;
    let b_ok = b.magnitude == 0.0;
    if a_ok != b_ok {
        return a_ok;
    }
    if (a.magnitude - b.magnitude).abs() > 1e-9 {
        return a.magnitude < b.magnitude;
    }
    if a.net_union != b.net_union {
        return a.net_union < b.net_union;
    }
    lex_better_schedule(pool, &a.indices, &b.indices)
}

/// When every recipe must be used, multi-start only reorders. Build one order
/// greedily from the lexicographically smallest (title, id) seed.
fn order_full_pool(input: &GreedyInput<'_>) -> Vec<usize> {
    let seed = (0..input.pool.len())
        .min_by_key(|&i| (input.pool[i].title.as_str(), input.pool[i].id.as_str()))
        .expect("non-empty pool");
    greedy_from_seed(input, seed, input.pool.len())
}

/// Violations for a saved plan against bounds using precomputed recipe macros.
pub fn plan_bound_violations(
    _pool: &[Recipe],
    plan: &MealPlan,
    bounds: &NutritionBounds,
    recipe_macros: &HashMap<RecipeId, Macros>,
) -> Vec<BoundViolation> {
    if bounds.is_empty() {
        return Vec::new();
    }
    let mut per_day_map: std::collections::BTreeMap<u32, Macros> =
        std::collections::BTreeMap::new();
    let mut per_meal: Vec<(u32, u32, Macros)> = Vec::new();
    let mut plan_total = Macros::default();
    for m in &plan.meals {
        let macros = recipe_macros.get(&m.recipe_id).copied().unwrap_or_default();
        per_meal.push((m.day, m.meal, macros));
        plan_total.add(&macros);
        per_day_map.entry(m.day).or_default().add(&macros);
    }
    let per_day: Vec<(u32, Macros)> = per_day_map.into_iter().collect();
    evaluate_schedule(bounds, &per_day, &per_meal, &plan_total)
}

fn format_violations(violations: &[BoundViolation]) -> String {
    violations
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Build a meal plan from a candidate pool (no recipe repeats by id or
/// normalized title).
///
/// When `opts.pantry` is non-empty, those ingredient keys are treated as already
/// covered: they do not count as new when scoring, and the reported distinct
/// count is net of pantry stock.
///
/// When `opts.nutrition` is non-empty, selection prefers schedules that satisfy
/// macro min/max bounds (estimated whole-recipe macros in `opts.recipe_macros`).
/// If no feasible schedule exists, the least-violation plan is returned and
/// violations are listed in the rationale.
pub fn plan_meals(pool: &[Recipe], opts: &PlanOptions) -> MealPlan {
    let slots = opts
        .days
        .checked_mul(opts.meals_per_day)
        .map(|n| n as usize)
        .unwrap_or(0);
    let plan_id = uuid::Uuid::new_v4().to_string();
    let pantry = &opts.pantry;
    let bounds = &opts.nutrition;

    let (pool, keys) = normalize_pool(pool);
    let macros = align_macros(&pool, opts);

    if pool.is_empty() || slots == 0 {
        return MealPlan {
            id: plan_id,
            days: opts.days,
            meals_per_day: opts.meals_per_day,
            meals: vec![],
            rationale: "Empty pool or zero slots; no meals planned.".into(),
        };
    }

    let target = slots.min(pool.len());
    let mpd = opts.meals_per_day.max(1);
    let input = GreedyInput {
        pool: &pool,
        keys: &keys,
        macros: &macros,
        pantry,
        bounds,
        meals_per_day: mpd,
    };

    let scored = if target == pool.len() && bounds.is_empty() {
        let indices = order_full_pool(&input);
        score_schedule(&pool, &keys, &macros, indices, pantry, bounds, mpd)
    } else if bounds.is_empty() {
        let mut best: Option<(Vec<usize>, usize)> = None;
        for seed in 0..pool.len() {
            let candidate = greedy_from_seed(&input, seed, target);
            let ua = net_union_size(&keys, &candidate, pantry);
            let take = match &best {
                None => true,
                Some((b, ub)) => better_schedule(&pool, &candidate, ua, b, *ub),
            };
            if take {
                best = Some((candidate, ua));
            }
        }
        let indices = best.map(|(s, _)| s).unwrap_or_default();
        score_schedule(&pool, &keys, &macros, indices, pantry, bounds, mpd)
    } else {
        let mut best: Option<ScoredSchedule> = None;
        for seed in 0..pool.len() {
            let indices = greedy_from_seed(&input, seed, target);
            let candidate = score_schedule(&pool, &keys, &macros, indices, pantry, bounds, mpd);
            let take = match &best {
                None => true,
                Some(b) => better_scored(&pool, &candidate, b),
            };
            if take {
                best = Some(candidate);
            }
        }
        best.unwrap_or_else(|| ScoredSchedule {
            indices: Vec::new(),
            net_union: 0,
            magnitude: 0.0,
            violations: Vec::new(),
        })
    };

    let selected = &scored.indices;
    let total_unique = union_size(&keys, selected);
    let net_unique = scored.net_union;

    let meals: Vec<PlannedMeal> = selected
        .iter()
        .enumerate()
        .map(|(idx, &ri)| {
            let day = (idx as u32) / mpd;
            let meal = (idx as u32) % mpd;
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
            " Pool has only {} unique non-empty recipe(s); requested {} slot(s), so the plan is partial (repeats are never used).",
            pool.len(),
            slots
        )
    } else {
        String::new()
    };

    let pantry_note = if pantry.is_empty() {
        format!("Plan uses {total_unique} distinct ingredient key(s).")
    } else {
        format!(
            "Plan uses {total_unique} distinct ingredient key(s) \
             ({net_unique} not already in pantry; {} pantry key(s) considered).",
            pantry.len()
        )
    };

    let nutrition_note = if bounds.is_empty() {
        String::new()
    } else if scored.violations.is_empty() {
        " Nutrition constraints satisfied.".to_string()
    } else {
        format!(
            " Nutrition constraint violations (best effort): {}.",
            format_violations(&scored.violations)
        )
    };

    let rationale = format!(
        "Min-union planner: {} meal(s) over {} day(s) from a pool of {} unique recipe(s). \
         Multi-start greedy selection minimizes distinct ingredient keys \
         (no recipe repeats). {pantry_note}{nutrition_note}{partial_note}",
        meals.len(),
        opts.days,
        pool.len(),
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
    use crate::domain::UnitKind;
    use crate::normalize::normalize_line;

    fn rec(title: &str, ings: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    fn rec_with_id(id: &str, title: &str, ings: &[&str]) -> Recipe {
        let mut r = rec(title, ings);
        r.id = RecipeId::from(id);
        r
    }

    fn titles(plan: &MealPlan) -> Vec<&str> {
        plan.meals.iter().map(|m| m.recipe_title.as_str()).collect()
    }

    fn unique_recipe_ids(plan: &MealPlan) -> usize {
        plan.meals
            .iter()
            .map(|m| m.recipe_id.as_str())
            .collect::<HashSet<_>>()
            .len()
    }

    #[test]
    fn prefers_compact_shared_cluster_over_disjoint() {
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
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 2);
        let t = titles(&plan);
        assert!(t.contains(&"Pancakes"));
        assert!(t.contains(&"French Toast"));
        assert!(!t.contains(&"Spice Mix"));
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
                meals_per_day: 2, // 4 slots, only 2 recipes,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 2);
        assert_eq!(unique_recipe_ids(&plan), 2);
        assert!(plan.rationale.contains("partial"));
    }

    #[test]
    fn duplicate_pool_entries_do_not_repeat_by_id() {
        let a = rec_with_id("id-a", "A", &["1 cup rice"]);
        let pool = vec![a.clone(), a.clone(), a];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 3,
                meals_per_day: 1,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 1);
        assert_eq!(unique_recipe_ids(&plan), 1);
        assert_eq!(plan.meals[0].recipe_id.as_str(), "id-a");
    }

    #[test]
    fn duplicate_titles_different_ids_collapse() {
        let a = rec_with_id("id-a", "Grilled S'mores", &["1 bread"]);
        let b = rec_with_id("id-b", "grilled s'mores", &["1 bread", "1 chocolate"]);
        let plan = plan_meals(
            &[a, b],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 1);
        assert_eq!(plan.meals[0].recipe_id.as_str(), "id-a"); // first wins
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
                ..Default::default()
            },
        );
        assert!(plan.meals.is_empty());
    }

    #[test]
    fn overflow_slots_treated_as_empty() {
        let a = rec("A", &["1 cup water"]);
        let plan = plan_meals(
            &[a],
            &PlanOptions {
                days: u32::MAX,
                meals_per_day: 2,
                ..Default::default()
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
            ..Default::default()
        };
        let p1 = plan_meals(&pool1, &opts);
        let p2 = plan_meals(&pool2, &opts);
        assert_eq!(p1.meals[0].recipe_title, p2.meals[0].recipe_title);
        assert_eq!(p1.meals[0].recipe_title, "Alpha");
    }

    #[test]
    fn schedule_order_independent_of_pool_order() {
        let a = rec("Pancakes", &["2 cups flour", "1 cup milk", "2 eggs"]);
        let b = rec("French Toast", &["4 slices bread", "1 cup milk", "2 eggs"]);
        let c = rec("Spice Mix", &["1 tsp cumin", "1 tsp coriander"]);
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 1,
            ..Default::default()
        };
        let p1 = plan_meals(&[a.clone(), b.clone(), c.clone()], &opts);
        let p2 = plan_meals(&[c, b, a], &opts);
        // Full ordered sequence, not merely the set of titles.
        assert_eq!(titles(&p1), titles(&p2));
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
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 3);
        assert_eq!(unique_recipe_ids(&plan), 3);
        assert_eq!(plan_union_size(&pool, &plan), 2);
    }

    #[test]
    fn multi_start_avoids_bad_singleton_seed() {
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
                ..Default::default()
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
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 5);
        assert_eq!(unique_recipe_ids(&plan), 5);
    }

    #[test]
    fn day_and_meal_indices_pack_row_major() {
        let pool: Vec<_> = (0..4)
            .map(|i| rec(&format!("R{i}"), &["1 cup flour"]))
            .collect();
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 2,
                meals_per_day: 2,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 4);
        let pairs: Vec<_> = plan.meals.iter().map(|m| (m.day, m.meal)).collect();
        assert_eq!(pairs, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn empty_ingredient_recipes_dropped_when_nonempty_exist() {
        let empty = rec("Stub", &[]);
        let real = rec("Soup", &["1 cup broth", "1 onion"]);
        let plan = plan_meals(
            &[empty, real.clone()],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 1);
        assert_eq!(plan.meals[0].recipe_title, "Soup");
    }

    /// Empty stub must not title-claim ahead of a fuller same-title recipe
    /// (different ids). Regression: title first-wins before empty filter
    /// dropped both when the stub arrived first.
    #[test]
    fn empty_stub_does_not_block_fuller_same_title() {
        let empty = rec_with_id("stub", "S'mores", &[]);
        let full = rec_with_id(
            "full",
            "S'mores",
            &["1 graham cracker", "1 chocolate", "1 marshmallow"],
        );
        let other = rec_with_id("other", "Soup", &["1 cup broth"]);
        let plan = plan_meals(
            &[empty, full, other],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                ..Default::default()
            },
        );
        let ids: HashSet<_> = plan.meals.iter().map(|m| m.recipe_id.as_str()).collect();
        assert!(
            ids.contains("full"),
            "pool/plan must retain fuller S'mores, got ids {ids:?}"
        );
        assert!(!ids.contains("stub"));
        assert_eq!(plan.meals.len(), 2);
    }

    #[test]
    fn all_empty_recipes_still_schedule_without_repeats() {
        let a = rec_with_id("e1", "Empty A", &[]);
        let b = rec_with_id("e2", "Empty B", &[]);
        let plan = plan_meals(
            &[a, b],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 2);
        assert_eq!(unique_recipe_ids(&plan), 2);
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
            ..Default::default()
        };
        let p1 = plan_meals(&[a.clone(), b.clone(), c.clone()], &opts);
        let p2 = plan_meals(&[c, b, a], &opts);
        let mut s1: Vec<_> = titles(&p1);
        let mut s2: Vec<_> = titles(&p2);
        s1.sort();
        s2.sort();
        assert_eq!(s1, s2);
    }

    #[test]
    fn pantry_prefers_recipe_using_only_stocked_ingredients() {
        // Without pantry, Omelette+Scramble (2 keys) beat Spice+Salt paths.
        // With all spice ingredients stocked, "Spice Mix" alone costs 0 new keys
        // and a second spice-adjacent recipe would still be cheap — but more
        // simply: stock flour/milk/eggs/bread so breakfast cluster is free, then
        // the planner can still pick it; instead stock the exotic cluster so it
        // becomes free and is preferred when we need only 1 meal.
        let breakfast = rec("Pancakes", &["2 cups flour", "1 cup milk", "2 eggs"]);
        let spice = rec(
            "Spice Mix",
            &["1 tsp cumin", "1 tsp coriander", "1 tsp turmeric"],
        );
        let pool = vec![breakfast, spice];
        let pantry: HashSet<_> = [
            IngredientKey::new("cumin", UnitKind::Volume),
            IngredientKey::new("coriander", UnitKind::Volume),
            IngredientKey::new("turmeric", UnitKind::Volume),
        ]
        .into_iter()
        .collect();
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
            pantry,
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals.len(), 1);
        assert_eq!(plan.meals[0].recipe_title, "Spice Mix");
        assert!(plan.rationale.contains("pantry"));
    }

    #[test]
    fn pantry_reduces_reported_net_unique_count() {
        let a = rec("Pancakes", &["2 cups flour", "1 cup milk", "2 eggs"]);
        let b = rec("French Toast", &["4 slices bread", "1 cup milk", "2 eggs"]);
        let pool = vec![a, b];
        let pantry: HashSet<_> = [
            IngredientKey::new("milk", UnitKind::Volume),
            IngredientKey::new("eggs", UnitKind::Count),
        ]
        .into_iter()
        .collect();
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 1,
            pantry,
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        // Full union is 4 (flour, milk, eggs, bread); net after pantry is 2.
        assert!(
            plan.rationale.contains("2") && plan.rationale.to_lowercase().contains("pantry"),
            "rationale should report net uniques with pantry: {}",
            plan.rationale
        );
        assert_eq!(plan_union_size(&pool, &plan), 4);
    }

    #[test]
    fn empty_pantry_matches_prior_behavior() {
        let a = rec("Pancakes", &["2 cups flour", "1 cup milk", "2 eggs"]);
        let b = rec("French Toast", &["4 slices bread", "1 cup milk", "2 eggs"]);
        let c = rec(
            "Spice Mix",
            &["1 tsp cumin", "1 tsp coriander", "1 tsp turmeric"],
        );
        let pool = vec![a, b, c];
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 1,
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        let t = titles(&plan);
        assert!(t.contains(&"Pancakes"));
        assert!(t.contains(&"French Toast"));
    }

    fn macro_map(entries: &[(&str, Macros)]) -> HashMap<RecipeId, Macros> {
        entries
            .iter()
            .map(|(id, m)| (RecipeId::from(*id), *m))
            .collect()
    }

    fn dessert_protein_pool() -> (Vec<Recipe>, HashMap<RecipeId, Macros>) {
        let dessert1 = rec_with_id("d1", "Cake", &["100 g sugar", "100 g flour"]);
        let dessert2 = rec_with_id("d2", "Cookies", &["100 g sugar", "50 g flour"]);
        let protein = rec_with_id("p1", "Chicken Rice", &["200 g chicken", "100 g rice"]);
        let macros = macro_map(&[
            (
                "d1",
                Macros {
                    kcal: 800.0,
                    protein_g: 5.0,
                    fat_g: 10.0,
                    carbs_g: 150.0,
                },
            ),
            (
                "d2",
                Macros {
                    kcal: 700.0,
                    protein_g: 4.0,
                    fat_g: 20.0,
                    carbs_g: 100.0,
                },
            ),
            (
                "p1",
                Macros {
                    kcal: 500.0,
                    protein_g: 60.0,
                    fat_g: 10.0,
                    carbs_g: 40.0,
                },
            ),
        ]);
        (vec![dessert1, dessert2, protein], macros)
    }

    #[test]
    fn unconstrained_still_prefers_shared_dessert_cluster() {
        let (pool, macros) = dessert_protein_pool();
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 1,
                meals_per_day: 2,
                recipe_macros: macros,
                ..Default::default()
            },
        );
        let t = titles(&plan);
        assert!(t.contains(&"Cake"));
        assert!(t.contains(&"Cookies"));
        assert!(!t.contains(&"Chicken Rice"));
    }

    #[test]
    fn per_day_min_protein_avoids_two_desserts() {
        let (pool, macros) = dessert_protein_pool();
        let nutrition = NutritionBounds {
            per_day: MacroBounds {
                protein_g: MacroRange {
                    min: Some(50.0),
                    max: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 1,
                meals_per_day: 2,
                nutrition: nutrition.clone(),
                recipe_macros: macros.clone(),
                ..Default::default()
            },
        );
        let t = titles(&plan);
        assert!(
            t.contains(&"Chicken Rice"),
            "expected a protein meal, got {t:?}"
        );
        let violations = plan_bound_violations(&pool, &plan, &nutrition, &macros);
        assert!(
            violations.is_empty(),
            "expected feasible plan, got {violations:?}"
        );
        assert!(plan.rationale.to_lowercase().contains("nutrition"));
        assert!(!plan.rationale.to_lowercase().contains("violation"));
    }

    #[test]
    fn infeasible_bounds_return_best_effort_with_violations() {
        let dessert1 = rec_with_id("d1", "Cake", &["100 g sugar"]);
        let dessert2 = rec_with_id("d2", "Cookies", &["100 g sugar", "50 g flour"]);
        let macros = macro_map(&[
            (
                "d1",
                Macros {
                    protein_g: 5.0,
                    ..Default::default()
                },
            ),
            (
                "d2",
                Macros {
                    protein_g: 4.0,
                    ..Default::default()
                },
            ),
        ]);
        let nutrition = NutritionBounds {
            per_day: MacroBounds {
                protein_g: MacroRange {
                    min: Some(50.0),
                    max: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let pool = vec![dessert1, dessert2];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 1,
                meals_per_day: 2,
                nutrition: nutrition.clone(),
                recipe_macros: macros.clone(),
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 2);
        let violations = plan_bound_violations(&pool, &plan, &nutrition, &macros);
        assert!(!violations.is_empty());
        assert!(plan.rationale.to_lowercase().contains("violation"));
    }

    #[test]
    fn per_meal_min_filters_too_low_protein_recipe() {
        let low = rec_with_id("low", "Broth", &["1 cup broth"]);
        let high = rec_with_id("high", "Steak", &["200 g beef"]);
        let other = rec_with_id("other", "Eggs", &["3 eggs"]);
        let macros = macro_map(&[
            (
                "low",
                Macros {
                    protein_g: 2.0,
                    ..Default::default()
                },
            ),
            (
                "high",
                Macros {
                    protein_g: 40.0,
                    ..Default::default()
                },
            ),
            (
                "other",
                Macros {
                    protein_g: 18.0,
                    ..Default::default()
                },
            ),
        ]);
        let nutrition = NutritionBounds {
            per_meal: MacroBounds {
                protein_g: MacroRange {
                    min: Some(15.0),
                    max: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let pool = vec![low, high, other];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                nutrition: nutrition.clone(),
                recipe_macros: macros.clone(),
                ..Default::default()
            },
        );
        let t = titles(&plan);
        assert!(
            !t.contains(&"Broth"),
            "low-protein meal should be avoided: {t:?}"
        );
        let violations = plan_bound_violations(&pool, &plan, &nutrition, &macros);
        assert!(violations.is_empty(), "{violations:?}");
    }
}
