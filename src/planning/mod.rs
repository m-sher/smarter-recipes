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
//! Optional **pantry** stock is applied with **binary shortfall** semantics
//! shared with [`crate::shopping`]: a key counts as needing to buy iff demand
//! exceeds on-hand quantity after virtual consumption (exact key, then
//! mass↔volume density bridge). Partial stock no longer fully exempts a key.
//! Lines with no parsed quantity use a presence-only fallback (any positive
//! bridged stock covers; otherwise to-buy). Persisted pantry is never mutated.
//!
//! Optional [`NutritionBounds`] steer selection using precomputed whole-recipe
//! estimated [`Macros`]: prefer schedules that satisfy per-meal, per-day, and
//! plan-total min/max ranges; if none are feasible, return the least total
//! violation (then min-union). The rationale records a short status; callers
//! render the full violation list separately (e.g. CLI summary).
//!
//! When [`PlanOptions::recipe_macros`] contains an estimate for a recipe with
//! `kcal <= 0`, that recipe is dropped from the pool entirely (not a meal).
//! Recipes omitted from the map are left untouched for callers that do not
//! compute estimates.
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
//!    Running state is a cloned pantry ledger plus the set of keys already
//!    marked to-buy. While fewer than `S` recipes are selected, append the
//!    unused candidate that minimizes the number of **new** to-buy keys
//!    (quantity shortfall or missing presence). Ties break by:
//!    - smaller `|keys(candidate)|` (prefer compact recipes),
//!    - then lexicographically smaller title,
//!    - then lexicographically smaller `recipe_id` (full pool-order independence).
//!
//!    With nutrition bounds, infeasible per-meal/day-max candidates are skipped
//!    when a feasible alternative exists; deficit to per-day mins is a further
//!    tie-break.
//!
//! 4. **Multi-start** — run the greedy growth once for **every** pool member as
//!    seed. Keep the schedule with the smallest final **net** to-buy size (or,
//!    with bounds, feasible first, then least violation magnitude, then net
//!    to-buy). Ties break by lex `(title, recipe_id)` sequence. Equal schedules
//!    keep the incumbent. When `S == pool.len()` and bounds are empty,
//!    multi-start is skipped and a single greedy order is built from the
//!    lex-smallest seed.
//!
//! Construction order is the plan order: ingredients tend to appear when first
//! needed, which keeps [`crate::shopping::trip_breakdown_for_plan`] meaningful.
//!
//! Complexity: O(P² · S · K) where P = pool size, S = slots, K = avg keys/recipe
//! — fine for tens to low hundreds of recipes.

mod ilp;
mod nutrition_bounds;

pub use nutrition_bounds::{
    evaluate_macros, evaluate_schedule, exceeds_max, load_nutrition_bounds, min_deficit,
    violates_per_meal, violation_magnitude, weighted_magnitude, BoundScope, BoundViolation,
    CliPerDayNutrition, MacroBounds, MacroRange, MacroRatio, NutrientKind, NutritionBounds,
    ViolationKind, KCAL_WEIGHT, MACRO_WEIGHT, RATIO_WEIGHT,
};

use crate::domain::{
    normalize_title_key, IngredientKey, Macros, MealPlan, PantryItem, PlannedMeal, Recipe, RecipeId,
};
use crate::shopping::{consume_from_stock, pantry_quantity_for};
use std::collections::{HashMap, HashSet};

/// Quantity comparison tolerance; matches [`crate::shopping`] shortfall checks.
const EPS: f64 = 1e-9;

/// Minimum fraction of a recipe's estimable ingredients that must have a resolved
/// macro profile for the recipe to be usable under nutrition bounds. Below this,
/// the estimate understates reality by too much to trust against a constraint.
pub const MIN_INGREDIENT_COVERAGE: f64 = 0.75;

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub days: u32,
    pub meals_per_day: u32,
    /// On-hand stock in canonical units; consumed virtually while scoring.
    pub pantry: Vec<PantryItem>,
    /// Optional macro min/max constraints. Empty keeps legacy min-union behavior.
    pub nutrition: NutritionBounds,
    /// Precomputed whole-recipe estimated macros (missing ids treat as zero).
    pub recipe_macros: HashMap<RecipeId, Macros>,
    /// Recipes whose ingredient coverage is below [`MIN_INGREDIENT_COVERAGE`], so
    /// their macro totals understate reality. Excluded from the pool only when
    /// nutrition bounds are configured (an unreliable estimate can't be trusted
    /// against a constraint); ignored for unconstrained min-union planning.
    pub recipe_low_coverage: HashSet<RecipeId>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            days: 7,
            meals_per_day: 1,
            pantry: Vec::new(),
            nutrition: NutritionBounds::default(),
            recipe_macros: HashMap::new(),
            recipe_low_coverage: HashSet::new(),
        }
    }
}

type RecipeReq = Vec<(IngredientKey, f64)>;
type NormalizedPool<'a> = (
    Vec<&'a Recipe>,
    Vec<RecipeReq>,
    Vec<HashSet<IngredientKey>>,
    // (dropped as non-meals, dropped for incomplete nutrition estimates)
    usize,
    usize,
);

/// Aggregate per-recipe requirements in canonical units. Missing quantities are
/// recorded as `0.0` (presence-only sentinel); never invent amounts.
fn recipe_requirements(recipe: &Recipe) -> RecipeReq {
    let mut map: HashMap<IngredientKey, f64> = HashMap::new();
    for line in &recipe.ingredients {
        let key = IngredientKey::from_line(line);
        if let Some((canon, _)) = line.canonical_quantity() {
            *map.entry(key).or_insert(0.0) += canon;
        } else {
            map.entry(key).or_insert(0.0);
        }
    }
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by(|a, b| {
        a.0.name
            .cmp(&b.0.name)
            .then_with(|| (a.0.kind as u8).cmp(&(b.0.kind as u8)))
    });
    v
}

fn recipe_keys_from_reqs(reqs: &[(IngredientKey, f64)]) -> HashSet<IngredientKey> {
    reqs.iter().map(|(k, _)| k.clone()).collect()
}

fn recipe_keys(recipe: &Recipe) -> HashSet<IngredientKey> {
    recipe_keys_from_reqs(&recipe_requirements(recipe))
}

/// Apply one recipe's requirements to a mutable coverage state. Returns how many
/// keys were newly marked to-buy. Consumes stock for positive needs (including
/// when the key was already to-buy) so later meals see depleted quantities.
fn apply_recipe_to_coverage(
    stock: &mut [PantryItem],
    to_buy: &mut HashSet<IngredientKey>,
    reqs: &[(IngredientKey, f64)],
) -> usize {
    let mut new_count = 0;
    for (key, need) in reqs {
        if *need > EPS {
            let shortfall = consume_from_stock(stock, key, *need);
            if shortfall > EPS && to_buy.insert(key.clone()) {
                new_count += 1;
            }
        } else if pantry_quantity_for(key, stock) > EPS {
            // Presence-only line covered by any positive bridged stock.
        } else if to_buy.insert(key.clone()) {
            new_count += 1;
        }
    }
    new_count
}

/// True when a precomputed estimate exists but the recipe cannot serve as a
/// macro-characterizable meal: it reports no calories, or calories with no
/// protein/fat/carbs at all (e.g. an alcohol-only recipe, whose kcal come from
/// ethanol). Such recipes are excluded so the planner cannot use them to
/// trivially "satisfy" a macro-split ratio — a scope with zero total macro
/// grams has no split to check, which the exact solver would otherwise exploit.
fn is_non_meal_estimate(recipe_macros: &HashMap<RecipeId, Macros>, id: &RecipeId) -> bool {
    recipe_macros.get(id).is_some_and(|m| {
        !m.kcal.is_finite() || m.kcal <= 0.0 || m.protein_g + m.fat_g + m.carbs_g <= 0.0
    })
}

/// Deduplicate by `recipe_id`, drop empty-ingredient recipes when any non-empty
/// recipe exists, drop recipes whose precomputed estimate cannot serve as a meal
/// (no calories, or calories with no macro breakdown), drop recipes in
/// `exclude_low_coverage` (below the coverage threshold; `Some` only when bounds
/// are configured), then collapse by normalized title key (first wins among
/// survivors). Empty filtering must run before title collapse so an empty stub
/// cannot claim a title and block a fuller same-title recipe. Returns recipes
/// paired with precomputed requirements and key sets, plus how many candidates
/// were removed as non-meals and how many for low coverage.
fn normalize_pool<'a>(
    pool: &'a [Recipe],
    recipe_macros: &HashMap<RecipeId, Macros>,
    exclude_low_coverage: Option<&HashSet<RecipeId>>,
) -> NormalizedPool<'a> {
    let mut seen_ids = HashSet::new();
    let mut recipes: Vec<&Recipe> = Vec::new();
    let mut reqs: Vec<RecipeReq> = Vec::new();
    let mut keys: Vec<HashSet<IngredientKey>> = Vec::new();
    for r in pool {
        if !seen_ids.insert(r.id.as_str()) {
            continue;
        }
        let r_reqs = recipe_requirements(r);
        let r_keys = recipe_keys_from_reqs(&r_reqs);
        recipes.push(r);
        reqs.push(r_reqs);
        keys.push(r_keys);
    }

    let any_nonempty = keys.iter().any(|ks| !ks.is_empty());
    if any_nonempty {
        let mut i = 0;
        while i < recipes.len() {
            if keys[i].is_empty() {
                recipes.remove(i);
                reqs.remove(i);
                keys.remove(i);
            } else {
                i += 1;
            }
        }
    }

    let mut dropped_non_meal = 0usize;
    let mut dropped_low_coverage = 0usize;
    let mut i = 0;
    while i < recipes.len() {
        let id = &recipes[i].id;
        if is_non_meal_estimate(recipe_macros, id) {
            dropped_non_meal += 1;
        } else if exclude_low_coverage.is_some_and(|set| set.contains(id)) {
            dropped_low_coverage += 1;
        } else {
            i += 1;
            continue;
        }
        recipes.remove(i);
        reqs.remove(i);
        keys.remove(i);
    }

    let mut seen_titles = HashSet::new();
    let mut out_recipes: Vec<&Recipe> = Vec::new();
    let mut out_reqs: Vec<RecipeReq> = Vec::new();
    let mut out_keys: Vec<HashSet<IngredientKey>> = Vec::new();
    for ((r, r_reqs), k) in recipes.into_iter().zip(reqs).zip(keys) {
        let title_key = normalize_title_key(&r.title);
        if !title_key.is_empty() && !seen_titles.insert(title_key) {
            continue;
        }
        out_recipes.push(r);
        out_reqs.push(r_reqs);
        out_keys.push(k);
    }
    (
        out_recipes,
        out_reqs,
        out_keys,
        dropped_non_meal,
        dropped_low_coverage,
    )
}

/// Full union size of ingredient keys for recipes at the given pool indices.
fn union_size(keys: &[HashSet<IngredientKey>], indices: &[usize]) -> usize {
    let mut u: HashSet<&IngredientKey> = HashSet::new();
    for &i in indices {
        u.extend(keys[i].iter());
    }
    u.len()
}

/// Keys in the selected recipes that still need sourcing after quantity-aware
/// pantry consumption (binary shortfall / missing presence).
fn net_shortfall_count(reqs: &[RecipeReq], indices: &[usize], pantry: &[PantryItem]) -> usize {
    let mut stock = pantry.to_vec();
    let mut to_buy = HashSet::new();
    for &i in indices {
        apply_recipe_to_coverage(&mut stock, &mut to_buy, &reqs[i]);
    }
    to_buy.len()
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

#[allow(clippy::too_many_arguments)]
fn candidate_sort_key<'a>(
    pool: &[&'a Recipe],
    reqs: &[RecipeReq],
    keys: &[HashSet<IngredientKey>],
    stock: &[PantryItem],
    to_buy: &HashSet<IngredientKey>,
    macros: &[Macros],
    bounds: &NutritionBounds,
    day_macros: &Macros,
    i: usize,
) -> (usize, usize, u64, &'a str, &'a str) {
    let mut stock_c = stock.to_vec();
    let mut to_buy_c = to_buy.clone();
    let new_keys = apply_recipe_to_coverage(&mut stock_c, &mut to_buy_c, &reqs[i]);
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
    reqs: &'a [RecipeReq],
    keys: &'a [HashSet<IngredientKey>],
    macros: &'a [Macros],
    pantry: &'a [PantryItem],
    bounds: &'a NutritionBounds,
    meals_per_day: u32,
}

/// Greedy growth from `seed`: always add the unused recipe that introduces the
/// fewest new to-buy keys (relative to remaining pantry stock + already
/// selected). Returns selected pool indices in plan order.
fn greedy_from_seed(input: &GreedyInput<'_>, seed: usize, target: usize) -> Vec<usize> {
    let GreedyInput {
        pool,
        reqs,
        keys,
        macros,
        pantry,
        bounds,
        meals_per_day,
    } = input;
    let mpd = (*meals_per_day).max(1);
    let mut selected = Vec::with_capacity(target);
    selected.push(seed);
    let mut stock = pantry.to_vec();
    let mut to_buy = HashSet::new();
    apply_recipe_to_coverage(&mut stock, &mut to_buy, &reqs[seed]);
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
            let key = candidate_sort_key(
                pool,
                reqs,
                keys,
                &stock,
                &to_buy,
                macros,
                bounds,
                &day_macros,
                i,
            );
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
        apply_recipe_to_coverage(&mut stock, &mut to_buy, &reqs[choice]);
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
    reqs: &[RecipeReq],
    macros: &[Macros],
    indices: Vec<usize>,
    pantry: &[PantryItem],
    bounds: &NutritionBounds,
    meals_per_day: u32,
) -> ScoredSchedule {
    let net_union = net_shortfall_count(reqs, &indices, pantry);
    let violations = if bounds.is_empty() {
        Vec::new()
    } else {
        schedule_violations(macros, &indices, bounds, meals_per_day)
    };
    // Rank by the weighted (calories + ratio prioritized) magnitude; the raw
    // violation list is still reported verbatim.
    let magnitude = weighted_magnitude(&violations);
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

/// Greedy fallback for the constrained path: multi-start greedy ranked by
/// [`better_scored`]. Used when the exact solver declines (too large, error, or
/// time budget). Never worse than the pre-ILP behavior.
fn greedy_constrained_scored(input: &GreedyInput<'_>, target: usize) -> ScoredSchedule {
    let mut best: Option<ScoredSchedule> = None;
    for seed in 0..input.pool.len() {
        let indices = greedy_from_seed(input, seed, target);
        let candidate = score_schedule(
            input.pool,
            input.reqs,
            input.macros,
            indices,
            input.pantry,
            input.bounds,
            input.meals_per_day,
        );
        let take = match &best {
            None => true,
            Some(b) => better_scored(input.pool, &candidate, b),
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

/// Build a meal plan from a candidate pool (no recipe repeats by id or
/// normalized title).
///
/// When `opts.pantry` is non-empty, on-hand quantities are consumed virtually
/// while scoring: keys with any shortfall (or missing presence for unquantified
/// lines) count toward the net to-buy cost.
///
/// When `opts.nutrition` is non-empty, selection prefers schedules that satisfy
/// macro min/max bounds (estimated whole-recipe macros in `opts.recipe_macros`).
/// If no feasible schedule exists, the least-violation plan is returned and
/// the rationale notes the violation count (details via [`plan_bound_violations`]).
pub fn plan_meals(pool: &[Recipe], opts: &PlanOptions) -> MealPlan {
    let slots = opts
        .days
        .checked_mul(opts.meals_per_day)
        .map(|n| n as usize)
        .unwrap_or(0);
    let plan_id = uuid::Uuid::new_v4().to_string();
    let pantry = &opts.pantry;
    let bounds = &opts.nutrition;

    // Recipes below the ingredient-coverage threshold are only dropped when
    // bounds are configured — an unconstrained plan doesn't care about macros.
    let exclude_low_coverage = (!bounds.is_empty()).then_some(&opts.recipe_low_coverage);
    let (pool, reqs, keys, dropped_non_meal, dropped_low_coverage) =
        normalize_pool(pool, &opts.recipe_macros, exclude_low_coverage);
    let macros = align_macros(&pool, opts);
    let coverage_pct = (MIN_INGREDIENT_COVERAGE * 100.0).round() as u32;

    if pool.is_empty() || slots == 0 {
        let rationale = if slots == 0 {
            "Empty pool or zero slots; no meals planned.".into()
        } else if dropped_non_meal > 0 {
            format!(
                "No meals planned: all {dropped_non_meal} candidate recipe(s) had no estimated calories or no macro breakdown, so none qualify as meals."
            )
        } else if dropped_low_coverage > 0 {
            format!(
                "No meals planned: all {dropped_low_coverage} candidate recipe(s) fall below the {coverage_pct}% ingredient-coverage threshold (units that can't be converted to grams, and no usable published nutrition). Relax the nutrition bounds, or add recipes that carry source nutrition."
            )
        } else {
            "Empty pool or zero slots; no meals planned.".into()
        };
        return MealPlan {
            id: plan_id,
            days: opts.days,
            meals_per_day: opts.meals_per_day,
            meals: vec![],
            rationale,
        };
    }

    let target = slots.min(pool.len());
    let mpd = opts.meals_per_day.max(1);
    let input = GreedyInput {
        pool: &pool,
        reqs: &reqs,
        keys: &keys,
        macros: &macros,
        pantry,
        bounds,
        meals_per_day: mpd,
    };

    // Track whether the exact solver produced the constrained plan (for the
    // rationale lead); the greedy fallback labels itself best-effort.
    let mut planner_ilp = false;
    let scored = if target == pool.len() && bounds.is_empty() {
        let indices = order_full_pool(&input);
        score_schedule(&pool, &reqs, &macros, indices, pantry, bounds, mpd)
    } else if bounds.is_empty() {
        let mut best: Option<(Vec<usize>, usize)> = None;
        for seed in 0..pool.len() {
            let candidate = greedy_from_seed(&input, seed, target);
            let ua = net_shortfall_count(&reqs, &candidate, pantry);
            let take = match &best {
                None => true,
                Some((b, ub)) => better_schedule(&pool, &candidate, ua, b, *ub),
            };
            if take {
                best = Some((candidate, ua));
            }
        }
        let indices = best.map(|(s, _)| s).unwrap_or_default();
        score_schedule(&pool, &reqs, &macros, indices, pantry, bounds, mpd)
    } else {
        // Non-empty bounds: solve the selection exactly, falling back to the
        // greedy planner when the solver declines.
        match ilp::solve_constrained(&input, target) {
            Some(indices) => {
                planner_ilp = true;
                score_schedule(&pool, &reqs, &macros, indices, pantry, bounds, mpd)
            }
            None => greedy_constrained_scored(&input, target),
        }
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

    let non_meal_note = if dropped_non_meal > 0 {
        format!(
            " Excluded {dropped_non_meal} recipe(s) with no estimated calories or no macro breakdown (not treated as meals)."
        )
    } else {
        String::new()
    };

    let low_coverage_note = if dropped_low_coverage > 0 {
        format!(
            " Excluded {dropped_low_coverage} recipe(s) below the {coverage_pct}% ingredient-coverage threshold (unconvertible units, no usable source nutrition)."
        )
    } else {
        String::new()
    };

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
             ({net_unique} not fully covered by pantry stock; {} pantry item(s) considered).",
            pantry.len()
        )
    };

    let nutrition_note = if bounds.is_empty() {
        String::new()
    } else if scored.violations.is_empty() {
        " Nutrition constraints satisfied.".to_string()
    } else {
        format!(
            " Nutrition constraints not fully met (best effort, {} violation(s)).",
            scored.violations.len()
        )
    };

    let rationale = if bounds.is_empty() {
        format!(
            "Min-union planner: {} meal(s) over {} day(s) from a pool of {} unique recipe(s), \
             no recipe repeats. {pantry_note}{non_meal_note}{nutrition_note}{partial_note}",
            meals.len(),
            opts.days,
            pool.len(),
        )
    } else {
        let lead = if planner_ilp {
            "Exact ILP planner"
        } else {
            "Best-effort planner"
        };
        format!(
            "{lead}: {} meal(s) over {} day(s) from a pool of {} unique recipe(s), \
             no recipe repeats. {pantry_note}{non_meal_note}{low_coverage_note}{nutrition_note}{partial_note}",
            meals.len(),
            opts.days,
            pool.len(),
        )
    };

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

    fn item(name: &str, kind: UnitKind, qty: f64) -> PantryItem {
        PantryItem {
            key: IngredientKey::new(name, kind),
            quantity_canonical: qty,
        }
    }

    fn stocked(name: &str, kind: UnitKind) -> PantryItem {
        item(name, kind, 1_000_000.0)
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
        let pantry = vec![
            stocked("cumin", UnitKind::Volume),
            stocked("coriander", UnitKind::Volume),
            stocked("turmeric", UnitKind::Volume),
        ];
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
        let pantry = vec![
            stocked("milk", UnitKind::Volume),
            stocked("eggs", UnitKind::Count),
        ];
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 1,
            pantry,
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        // Full union is 4 (flour, milk, eggs, bread); net after pantry is 2.
        assert!(
            plan.rationale.contains("2")
                && plan.rationale.to_lowercase().contains("not fully covered"),
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
        assert!(plan.rationale.to_lowercase().contains("satisfied"));
        assert!(!plan.rationale.contains("day "));
    }

    #[test]
    fn infeasible_bounds_return_best_effort_with_violations() {
        let dessert1 = rec_with_id("d1", "Cake", &["100 g sugar"]);
        let dessert2 = rec_with_id("d2", "Cookies", &["100 g sugar", "50 g flour"]);
        let macros = macro_map(&[
            (
                "d1",
                Macros {
                    kcal: 500.0,
                    protein_g: 5.0,
                    ..Default::default()
                },
            ),
            (
                "d2",
                Macros {
                    kcal: 400.0,
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
        assert!(plan.rationale.to_lowercase().contains("best effort"));
        assert!(
            plan.rationale
                .contains(&format!("{} violation(s)", violations.len())),
            "rationale should include violation count only, got: {}",
            plan.rationale
        );
        // Details belong in plan_bound_violations / CLI summary, not inline.
        assert!(!plan.rationale.contains("protein_g"));
    }

    #[test]
    fn zero_kcal_recipes_excluded_from_pool() {
        let real = rec_with_id("real", "Chicken", &["200 g chicken"]);
        let zero = rec_with_id("zero", "Wonton Guide", &["wonton wrappers"]);
        let also_zero = rec_with_id("z2", "Kabobs", &["8 pretzel sticks"]);
        let macros = macro_map(&[
            (
                "real",
                Macros {
                    kcal: 400.0,
                    protein_g: 50.0,
                    ..Default::default()
                },
            ),
            (
                "zero",
                Macros {
                    kcal: 0.0,
                    ..Default::default()
                },
            ),
            (
                "z2",
                Macros {
                    kcal: 0.0,
                    ..Default::default()
                },
            ),
        ]);
        let pool = vec![real, zero, also_zero];
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 3,
                meals_per_day: 1,
                recipe_macros: macros,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 1);
        assert_eq!(plan.meals[0].recipe_title, "Chicken");
        assert!(
            plan.rationale
                .to_lowercase()
                .contains("no estimated calories")
                || plan.rationale.to_lowercase().contains("zero kcal"),
            "rationale should mention exclusion: {}",
            plan.rationale
        );
        assert!(plan.rationale.contains("partial"));
    }

    #[test]
    fn missing_macro_entry_not_treated_as_zero_kcal() {
        // Legacy callers that omit recipe_macros keep prior scheduling behavior.
        let a = rec_with_id("a", "Guide", &["wonton wrappers"]);
        let b = rec_with_id("b", "Soup", &["1 cup broth"]);
        let plan = plan_meals(
            &[a, b],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 2);
    }

    #[test]
    fn all_zero_kcal_pool_plans_nothing() {
        let a = rec_with_id("a", "A", &["x"]);
        let b = rec_with_id("b", "B", &["y"]);
        let macros = macro_map(&[("a", Macros::default()), ("b", Macros::default())]);
        let plan = plan_meals(
            &[a, b],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                recipe_macros: macros,
                ..Default::default()
            },
        );
        assert!(plan.meals.is_empty());
        assert!(
            plan.rationale
                .to_lowercase()
                .contains("no estimated calories")
                || plan.rationale.to_lowercase().contains("zero kcal"),
            "{}",
            plan.rationale
        );
    }

    #[test]
    fn calories_without_macros_excluded_from_pool() {
        // An alcohol-only recipe carries kcal (from ethanol) but zero
        // protein/fat/carbs, so a macro-split ratio has no split to evaluate.
        // It must be excluded, or the exact solver would pick it to trivially
        // "satisfy" a ratio (and it isn't a nutritional meal anyway).
        let real = rec_with_id("real", "Chicken", &["200 g chicken"]);
        let booze = rec_with_id("booze", "Cocktail", &["3 oz vodka"]);
        let macros = macro_map(&[
            (
                "real",
                Macros {
                    kcal: 300.0,
                    protein_g: 40.0,
                    fat_g: 10.0,
                    carbs_g: 5.0,
                },
            ),
            (
                "booze",
                Macros {
                    kcal: 196.0,
                    ..Default::default()
                },
            ),
        ]);
        let plan = plan_meals(
            &[real, booze],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                recipe_macros: macros,
                ..Default::default()
            },
        );
        // Only the real recipe qualifies; the calorie-only recipe is dropped.
        assert_eq!(plan.meals.len(), 1);
        assert_eq!(plan.meals[0].recipe_title, "Chicken");
        assert!(
            plan.rationale.to_lowercase().contains("no macro breakdown"),
            "rationale should note the macro-less exclusion: {}",
            plan.rationale
        );
    }

    #[test]
    fn low_coverage_recipes_excluded_only_under_nutrition_bounds() {
        let covered = rec_with_id("cov", "Steak", &["200 g beef"]);
        let partial = rec_with_id("unc", "Mystery Stew", &["200 g beef", "1 dash unobtainium"]);
        let full = Macros {
            kcal: 400.0,
            protein_g: 40.0,
            fat_g: 20.0,
            carbs_g: 5.0,
        };
        let macros = macro_map(&[("cov", full), ("unc", full)]);
        let low_coverage: HashSet<RecipeId> = [RecipeId::from("unc")].into_iter().collect();
        let bounds = NutritionBounds {
            per_day: MacroBounds {
                protein_g: MacroRange {
                    min: Some(1.0),
                    max: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };

        // With bounds, the low-coverage recipe is dropped.
        let bounded = plan_meals(
            &[covered.clone(), partial.clone()],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                nutrition: bounds,
                recipe_macros: macros.clone(),
                recipe_low_coverage: low_coverage.clone(),
                ..Default::default()
            },
        );
        let titles: Vec<_> = bounded
            .meals
            .iter()
            .map(|m| m.recipe_title.as_str())
            .collect();
        assert!(
            titles.contains(&"Steak") && !titles.contains(&"Mystery Stew"),
            "{titles:?} / {}",
            bounded.rationale
        );
        assert!(bounded.rationale.contains("ingredient-coverage threshold"));

        // Without bounds, coverage is irrelevant — both recipes stay candidates.
        let unbounded = plan_meals(
            &[covered, partial],
            &PlanOptions {
                days: 2,
                meals_per_day: 1,
                recipe_macros: macros,
                recipe_low_coverage: low_coverage,
                ..Default::default()
            },
        );
        assert_eq!(unbounded.meals.len(), 2);
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
                    kcal: 50.0,
                    protein_g: 2.0,
                    ..Default::default()
                },
            ),
            (
                "high",
                Macros {
                    kcal: 400.0,
                    protein_g: 40.0,
                    ..Default::default()
                },
            ),
            (
                "other",
                Macros {
                    kcal: 200.0,
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
    #[test]
    fn sufficient_pantry_quantity_makes_recipe_free() {
        let with_flour = rec("Flour Cake", &["10 g flour"]);
        let with_rice = rec("Rice Bowl", &["10 g rice"]);
        let pool = vec![with_flour, with_rice];
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
            pantry: vec![item("flour", UnitKind::Mass, 20.0)],
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals[0].recipe_title, "Flour Cake");
        assert!(
            plan.rationale.contains("0")
                && plan.rationale.to_lowercase().contains("not fully covered"),
            "{}",
            plan.rationale
        );
    }

    #[test]
    fn insufficient_pantry_quantity_is_not_free() {
        let needs_more_flour = rec("Big Bread", &["20 g flour"]);
        let needs_rice = rec("Rice Bowl", &["10 g rice"]);
        let pool = vec![needs_more_flour, needs_rice];
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
            pantry: vec![item("flour", UnitKind::Mass, 10.0)],
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert!(
            plan.rationale.contains("1")
                && plan.rationale.to_lowercase().contains("not fully covered"),
            "partial stock must still count as to-buy: {}",
            plan.rationale
        );
        let opts_enough = PlanOptions {
            days: 1,
            meals_per_day: 1,
            pantry: vec![item("flour", UnitKind::Mass, 20.0)],
            ..Default::default()
        };
        let plan_enough = plan_meals(&pool, &opts_enough);
        assert_eq!(plan_enough.meals[0].recipe_title, "Big Bread");
        assert!(
            plan_enough.rationale.contains("0")
                && plan_enough
                    .rationale
                    .to_lowercase()
                    .contains("not fully covered"),
            "{}",
            plan_enough.rationale
        );
    }

    #[test]
    fn cross_recipe_depletion_marks_shared_ingredient() {
        let a = rec("A Loaf", &["15 g flour"]);
        let b = rec("B Loaf", &["15 g flour"]);
        let pool = vec![a, b];
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 1,
            pantry: vec![item("flour", UnitKind::Mass, 20.0)],
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals.len(), 2);
        assert!(
            plan.rationale.contains("1")
                && plan.rationale.to_lowercase().contains("not fully covered"),
            "{}",
            plan.rationale
        );
    }

    #[test]
    fn presence_only_line_uses_stock_presence() {
        let salty = rec("Salty", &["salt to taste"]);
        let spicy = rec("Spicy", &["1 tsp cumin"]);
        let pool = vec![salty, spicy];
        let with_salt = PlanOptions {
            days: 1,
            meals_per_day: 1,
            pantry: vec![item("salt", UnitKind::Count, 1.0)],
            ..Default::default()
        };
        assert_eq!(plan_meals(&pool, &with_salt).meals[0].recipe_title, "Salty");
        let empty = PlanOptions {
            days: 1,
            meals_per_day: 1,
            pantry: vec![],
            ..Default::default()
        };
        let plan_empty = plan_meals(&pool, &empty);
        assert_eq!(plan_empty.meals.len(), 1);
        assert!(
            plan_empty.rationale.contains("1 distinct ingredient"),
            "{}",
            plan_empty.rationale
        );
    }

    #[test]
    fn density_bridge_covers_volume_recipe_from_mass_stock() {
        let vol_flour = rec("Cupcake", &["1 cup flour"]);
        let rice = rec("Rice", &["1 cup rice"]);
        let pool = vec![vol_flour, rice];
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
            pantry: vec![item("flour", UnitKind::Mass, 500.0)],
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals[0].recipe_title, "Cupcake");
        assert!(
            plan.rationale.contains("0")
                && plan.rationale.to_lowercase().contains("not fully covered"),
            "{}",
            plan.rationale
        );
    }

    #[test]
    fn ilp_selects_protein_meal_exactly() {
        // Feasible per-day case: exactly 2 meals, must include the protein dish,
        // and the plan is feasible (drives the flat single-day ILP path).
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
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 2,
            nutrition: nutrition.clone(),
            recipe_macros: macros.clone(),
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals.len(), 2);
        assert!(titles(&plan).contains(&"Chicken Rice"));
        assert!(plan_bound_violations(&pool, &plan, &nutrition, &macros).is_empty());
        assert!(plan.rationale.contains("Exact ILP planner"));
    }

    fn balanced_protein_pool() -> (Vec<Recipe>, HashMap<RecipeId, Macros>) {
        let c1 = rec_with_id("c1", "Chicken A", &["200 g chicken", "100 g rice"]);
        let c2 = rec_with_id("c2", "Chicken B", &["200 g chicken", "100 g beans"]);
        let k1 = rec_with_id("k1", "Cake A", &["100 g sugar", "100 g flour"]);
        let k2 = rec_with_id("k2", "Cake B", &["100 g sugar", "50 g cocoa"]);
        let macros = macro_map(&[
            (
                "c1",
                Macros {
                    kcal: 500.0,
                    protein_g: 60.0,
                    ..Default::default()
                },
            ),
            (
                "c2",
                Macros {
                    kcal: 500.0,
                    protein_g: 60.0,
                    ..Default::default()
                },
            ),
            (
                "k1",
                Macros {
                    kcal: 800.0,
                    protein_g: 5.0,
                    ..Default::default()
                },
            ),
            (
                "k2",
                Macros {
                    kcal: 700.0,
                    protein_g: 4.0,
                    ..Default::default()
                },
            ),
        ]);
        (vec![c1, c2, k1, k2], macros)
    }

    #[test]
    fn ilp_partition_balances_protein_across_days() {
        // days=2, meals_per_day=2, per-day protein min forces a chicken on each
        // day (two desserts on a day would fail). Exercises the partition model.
        let (pool, macros) = balanced_protein_pool();
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
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 2,
            nutrition: nutrition.clone(),
            recipe_macros: macros.clone(),
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals.len(), 4);
        let violations = plan_bound_violations(&pool, &plan, &nutrition, &macros);
        assert!(
            violations.is_empty(),
            "each day must reach the protein min: {violations:?}"
        );

        // Pool-order independent: reversed input yields the same plan.
        let reversed: Vec<Recipe> = pool.iter().rev().cloned().collect();
        let plan2 = plan_meals(&reversed, &opts);
        assert_eq!(
            titles(&plan),
            titles(&plan2),
            "partition output must be pool-order independent"
        );
    }

    #[test]
    fn ilp_flat_deterministic_across_pool_order() {
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
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 2,
            nutrition,
            recipe_macros: macros,
            ..Default::default()
        };
        let p1 = plan_meals(&pool, &opts);
        let reversed: Vec<Recipe> = pool.iter().rev().cloned().collect();
        let p2 = plan_meals(&reversed, &opts);
        assert_eq!(titles(&p1), titles(&p2));
    }

    #[test]
    fn large_pool_solved_exactly() {
        // A large pool is solved exactly in full (no size cap, no shortlist, no
        // greedy fallback). Here one recipe hits the 40/30/30 split; the rest are
        // carb-heavy, so the exact solver must find and pick the matching one.
        let mut pool = Vec::new();
        let mut macros = HashMap::new();
        for i in 0..200u32 {
            let id = format!("r{i}");
            pool.push(rec_with_id(
                &id,
                &format!("R{i:03}"),
                &[&format!("1 cup ing{i}")],
            ));
            // Carb-heavy filler except one balanced recipe.
            let m = if i == 137 {
                Macros {
                    kcal: 400.0,
                    protein_g: 40.0,
                    fat_g: 30.0,
                    carbs_g: 30.0,
                }
            } else {
                Macros {
                    kcal: 400.0,
                    protein_g: 5.0,
                    fat_g: 5.0,
                    carbs_g: 90.0,
                }
            };
            macros.insert(RecipeId::from(id.as_str()), m);
        }
        let nutrition = NutritionBounds {
            per_day: MacroBounds {
                ratio: MacroRatio {
                    protein: Some(40.0),
                    fat: Some(30.0),
                    carb: Some(30.0),
                    tolerance: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = plan_meals(
            &pool,
            &PlanOptions {
                days: 1,
                meals_per_day: 1,
                nutrition: nutrition.clone(),
                recipe_macros: macros.clone(),
                ..Default::default()
            },
        );
        assert_eq!(plan.meals.len(), 1);
        assert_eq!(
            plan.meals[0].recipe_title, "R137",
            "the exact solver must find the one ratio-matching recipe: {}",
            plan.rationale
        );
        assert!(
            plan.rationale.contains("Exact ILP planner"),
            "{}",
            plan.rationale
        );
        assert!(plan_bound_violations(&pool, &plan, &nutrition, &macros).is_empty());
    }

    #[test]
    fn ilp_ratio_prefers_balanced_macros() {
        // A per-day macro-split target should steer selection to the balanced
        // recipe over a carb-heavy one (flat single-day path).
        let pool = vec![
            rec_with_id("bal", "Balanced Bowl", &["100 g mix"]),
            rec_with_id("sk", "Sugar Bomb", &["100 g sugar"]),
        ];
        let macros = macro_map(&[
            (
                "bal",
                Macros {
                    kcal: 500.0,
                    protein_g: 30.0,
                    fat_g: 30.0,
                    carbs_g: 40.0,
                },
            ),
            (
                "sk",
                Macros {
                    kcal: 500.0,
                    protein_g: 5.0,
                    fat_g: 5.0,
                    carbs_g: 90.0,
                },
            ),
        ]);
        let nutrition = NutritionBounds {
            per_day: MacroBounds {
                ratio: MacroRatio {
                    protein: Some(30.0),
                    fat: Some(30.0),
                    carb: Some(40.0),
                    tolerance: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
            nutrition: nutrition.clone(),
            recipe_macros: macros.clone(),
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(titles(&plan), vec!["Balanced Bowl"]);
        assert!(plan_bound_violations(&pool, &plan, &nutrition, &macros).is_empty());
    }

    #[test]
    fn ilp_ratio_infeasible_reports_best_effort() {
        let pool = vec![rec_with_id("sk", "Sugar Bomb", &["100 g sugar"])];
        let macros = macro_map(&[(
            "sk",
            Macros {
                kcal: 500.0,
                protein_g: 5.0,
                fat_g: 5.0,
                carbs_g: 90.0,
            },
        )]);
        let nutrition = NutritionBounds {
            per_day: MacroBounds {
                ratio: MacroRatio {
                    protein: Some(40.0),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
            nutrition: nutrition.clone(),
            recipe_macros: macros.clone(),
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals.len(), 1);
        let v = plan_bound_violations(&pool, &plan, &nutrition, &macros);
        assert!(
            v.iter().any(|x| x.kind == ViolationKind::RatioBelowTarget),
            "expected a ratio violation, got {v:?}"
        );
        assert!(plan.rationale.to_lowercase().contains("best effort"));
    }

    #[test]
    fn weighting_prefers_ratio_over_protein_min() {
        // "Protein Skew" meets the protein min but wrecks the split; "Balanced"
        // nails the split but misses the protein min. Raw grams would pick the
        // skewed one (smaller magnitude), but ratio is weighted ~5x, so the
        // planner keeps the split and sacrifices the protein min.
        let pool = vec![
            rec_with_id("skew", "Protein Skew", &["100 g whey"]),
            rec_with_id("bal", "Balanced", &["100 g mix"]),
        ];
        let macros = macro_map(&[
            (
                "skew",
                Macros {
                    kcal: 500.0,
                    protein_g: 50.0,
                    fat_g: 5.0,
                    carbs_g: 5.0,
                },
            ),
            (
                "bal",
                Macros {
                    kcal: 500.0,
                    protein_g: 20.0,
                    fat_g: 20.0,
                    carbs_g: 20.0,
                },
            ),
        ]);
        let nutrition = NutritionBounds {
            per_day: MacroBounds {
                protein_g: MacroRange {
                    min: Some(50.0),
                    max: None,
                },
                ratio: MacroRatio {
                    protein: Some(33.0),
                    fat: None,
                    carb: None,
                    tolerance: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let opts = PlanOptions {
            days: 1,
            meals_per_day: 1,
            nutrition: nutrition.clone(),
            recipe_macros: macros.clone(),
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(
            titles(&plan),
            vec!["Balanced"],
            "ratio (weighted) should win over the protein min"
        );
        let v = plan_bound_violations(&pool, &plan, &nutrition, &macros);
        assert!(
            v.iter()
                .any(|x| x.kind == ViolationKind::BelowMin && x.nutrient == NutrientKind::ProteinG),
            "chosen plan should miss the protein min: {v:?}"
        );
        assert!(
            !v.iter().any(|x| matches!(
                x.kind,
                ViolationKind::RatioBelowTarget | ViolationKind::RatioAboveTarget
            )),
            "chosen plan should satisfy the ratio: {v:?}"
        );
    }

    fn ratio_partition_pool() -> (Vec<Recipe>, HashMap<RecipeId, Macros>) {
        let pool = vec![
            rec_with_id("p1", "Protein A", &["200 g chicken"]),
            rec_with_id("p2", "Protein B", &["200 g turkey"]),
            rec_with_id("c1", "Carb A", &["200 g rice"]),
            rec_with_id("c2", "Carb B", &["200 g pasta"]),
        ];
        let macros = macro_map(&[
            (
                "p1",
                Macros {
                    kcal: 200.0,
                    protein_g: 40.0,
                    ..Default::default()
                },
            ),
            (
                "p2",
                Macros {
                    kcal: 200.0,
                    protein_g: 40.0,
                    ..Default::default()
                },
            ),
            (
                "c1",
                Macros {
                    kcal: 160.0,
                    carbs_g: 40.0,
                    ..Default::default()
                },
            ),
            (
                "c2",
                Macros {
                    kcal: 160.0,
                    carbs_g: 40.0,
                    ..Default::default()
                },
            ),
        ]);
        (pool, macros)
    }

    #[test]
    fn ilp_partition_ratio_balances_and_is_deterministic() {
        // days=2, mpd=2, per-day 50/50 protein/carb split forces each day to pair
        // one protein recipe with one carb recipe (partition ratio buckets).
        let (pool, macros) = ratio_partition_pool();
        let nutrition = NutritionBounds {
            per_day: MacroBounds {
                ratio: MacroRatio {
                    protein: Some(50.0),
                    fat: None,
                    carb: Some(50.0),
                    tolerance: None,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let opts = PlanOptions {
            days: 2,
            meals_per_day: 2,
            nutrition: nutrition.clone(),
            recipe_macros: macros.clone(),
            ..Default::default()
        };
        let plan = plan_meals(&pool, &opts);
        assert_eq!(plan.meals.len(), 4);
        assert!(
            plan_bound_violations(&pool, &plan, &nutrition, &macros).is_empty(),
            "each day should balance 50/50 protein/carb"
        );
        let reversed: Vec<Recipe> = pool.iter().rev().cloned().collect();
        let plan2 = plan_meals(&reversed, &opts);
        assert_eq!(titles(&plan), titles(&plan2));
    }
}
