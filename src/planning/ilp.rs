//! Exact integer-program selection for nutrition-constrained meal plans.
//!
//! The greedy planner in [`super`] minimizes the distinct-ingredient union but
//! only *steers* by nutrition. When bounds are configured we instead solve the
//! selection exactly with a small MILP (via [`microlp`], pure Rust):
//!
//! * **Variables** — one binary `x` per candidate recipe (flat model) or per
//!   `(recipe, day)` cell when a per-day bound must partition meals across days.
//!   Continuous `y[k] ∈ [0,1]` per non-pantry ingredient key, and continuous
//!   slacks that measure how far each scope total sits outside its min/max.
//! * **Objective** — a two-phase lexicographic solve: phase 1 minimizes the
//!   total **weighted** violation magnitude (calories and the macro-split ratio
//!   are prioritized ~5×, see [`super::weighted_magnitude`]), so a feasible plan
//!   is returned whenever one exists; phase 2 fixes that optimum (`V ≤ V* + ε`)
//!   and minimizes the net ingredient union `Σ y`. This mirrors the ranking
//!   contract in [`super::better_scored`] (feasible-first, then min weighted
//!   violation, then min union).
//!
//! The violation objective reproduces [`super::weighted_magnitude`] exactly:
//! per-meal bounds are a per-recipe constant, per-day/plan bounds are slacks on
//! the relevant totals (each scaled by its constraint weight). Only `x` is
//! integer; `y` and slacks are integral at the optimum on their own, which keeps
//! the branch-and-bound tree small.
//!
//! Returns `None` (caller falls back to the greedy planner) when the model is
//! too large, the solver errors, or it hits the time budget — so we never hang
//! and never return a plan worse than today's.

use std::collections::HashMap;
use std::time::Duration;

use microlp::{ComparisonOp, OptimizationDirection, Problem, Solution, StopReason, Variable};

use super::{
    evaluate_macros, weighted_magnitude, BoundScope, GreedyInput, MacroBounds, MacroRange,
    MacroRatio, NutritionBounds, KCAL_WEIGHT, MACRO_WEIGHT, RATIO_WEIGHT,
};
use crate::domain::{Macros, UnitKind};
use crate::shopping::pantry_quantity_for;

/// Wall-clock backstop per solve so a pathological instance can't hang.
const SOLVE_TIME_LIMIT: Duration = Duration::from_secs(5);
/// Skip the exact solve above this many integer (selection) variables and let
/// the caller fall back to the greedy planner.
const MAX_ILP_INT_VARS: usize = 120;
/// Slack allowed above the phase-1 optimum when minimizing the union in phase 2.
const VIOLATION_EPS: f64 = 1e-6;

#[derive(Clone, Copy)]
enum Phase {
    /// Minimize total nutrition-violation magnitude.
    One,
    /// Fix the phase-1 optimum and minimize the net ingredient union.
    Two,
}

fn nutrient_val(m: &Macros, i: usize) -> f64 {
    match i {
        0 => m.kcal,
        1 => m.protein_g,
        2 => m.fat_g,
        _ => m.carbs_g,
    }
}

fn nutrient_range(b: &MacroBounds, i: usize) -> MacroRange {
    match i {
        0 => b.kcal,
        1 => b.protein_g,
        2 => b.fat_g,
        _ => b.carbs_g,
    }
}

fn kind_rank(kind: UnitKind) -> u8 {
    match kind {
        UnitKind::Mass => 0,
        UnitKind::Volume => 1,
        UnitKind::Count => 2,
        UnitKind::Other => 3,
    }
}

/// Weighted (calories + ratio prioritized) magnitude by which `m` alone sits
/// outside `bounds` — a per-recipe constant matching [`super::weighted_magnitude`]
/// so the ILP objective agrees with the greedy ranking. Scope label is
/// irrelevant to the magnitude.
fn recipe_violation(bounds: &MacroBounds, m: &Macros) -> f64 {
    if bounds.is_empty() {
        return 0.0;
    }
    weighted_magnitude(&evaluate_macros(
        bounds,
        m,
        BoundScope::PerMeal { day: 0, meal: 0 },
    ))
}

/// Add min/max slacks for one nutrient on a scope total expressed as `terms`
/// (`(var, nutrient_value)`). `weight` is the ranking weight for this nutrient
/// (`KCAL_WEIGHT` vs `MACRO_WEIGHT`), applied to both the objective and the
/// phase-2 cap so the ILP minimizes the same weighted magnitude as the scorer.
fn add_range_slacks(
    prob: &mut Problem,
    terms: &[(Variable, f64)],
    range: &MacroRange,
    slack_obj: f64,
    weight: f64,
    viol: &mut Vec<(Variable, f64)>,
) {
    if let Some(min) = range.min {
        let s = prob.add_var(slack_obj * weight, (0.0, f64::INFINITY));
        let mut expr = terms.to_vec();
        expr.push((s, 1.0)); // total + s_below >= min
        prob.add_constraint(&expr, ComparisonOp::Ge, min);
        viol.push((s, weight));
    }
    if let Some(max) = range.max {
        let s = prob.add_var(slack_obj * weight, (0.0, f64::INFINITY));
        let mut expr = terms.to_vec();
        expr.push((s, -1.0)); // total - s_above <= max
        prob.add_constraint(&expr, ComparisonOp::Le, max);
        viol.push((s, weight));
    }
}

/// Weight for a min/max slack on nutrient index `i` (0 = kcal).
fn range_weight(i: usize) -> f64 {
    if i == 0 {
        KCAL_WEIGHT
    } else {
        MACRO_WEIGHT
    }
}

/// Add ratio-target slacks for a scope total whose macro grams are given, per
/// selection cell, as `(var, protein_g, fat_g, carbs_g)`. For each specified
/// macro share `t` (fraction of `base = protein+fat+carbs`) with tolerance
/// `tol`, a continuous slack `s >= |actual - t*base| - tol*base` measures grams
/// beyond the band — linear because `base` is linear in the selection vars.
fn add_ratio_slacks(
    prob: &mut Problem,
    macro_terms: &[(Variable, f64, f64, f64)],
    ratio: &MacroRatio,
    slack_obj: f64,
    viol: &mut Vec<(Variable, f64)>,
) {
    if ratio.is_empty() || macro_terms.is_empty() {
        return;
    }
    let tol = ratio.effective_tolerance() / 100.0;
    for (which, target) in [(0usize, ratio.protein), (1, ratio.fat), (2, ratio.carb)] {
        let Some(target_pct) = target else {
            continue;
        };
        let t = target_pct / 100.0;
        // Ratio is a headline constraint: weight the slack like the scorer.
        let s = prob.add_var(slack_obj * RATIO_WEIGHT, (0.0, f64::INFINITY));
        // above: s - actual + (t + tol)*base >= 0
        // below: s + actual + (tol - t)*base >= 0
        let mut above: Vec<(Variable, f64)> = Vec::with_capacity(macro_terms.len() + 1);
        let mut below: Vec<(Variable, f64)> = Vec::with_capacity(macro_terms.len() + 1);
        above.push((s, 1.0));
        below.push((s, 1.0));
        for &(var, pg, fg, cg) in macro_terms {
            let base = pg + fg + cg;
            let actual = match which {
                0 => pg,
                1 => fg,
                _ => cg,
            };
            above.push((var, -actual + (t + tol) * base));
            below.push((var, actual + (tol - t) * base));
        }
        prob.add_constraint(&above, ComparisonOp::Ge, 0.0);
        prob.add_constraint(&below, ComparisonOp::Ge, 0.0);
        viol.push((s, RATIO_WEIGHT));
    }
}

fn solve_checked(mut prob: Problem) -> Option<Solution> {
    prob.set_time_limit(SOLVE_TIME_LIMIT);
    match prob.solve() {
        Ok(sol) if *sol.stop_reason() == StopReason::Finished => Some(sol),
        _ => None,
    }
}

/// Exactly select `target` recipes from `input.pool` to satisfy the configured
/// nutrition bounds (feasible-first, then min union), returning pool indices in
/// plan (row-major) order. `None` means the caller should fall back to greedy.
pub(super) fn solve_constrained(input: &GreedyInput<'_>, target: usize) -> Option<Vec<usize>> {
    let p = input.pool.len();
    if target == 0 || p == 0 {
        return None;
    }
    let mpd = input.meals_per_day.max(1) as usize;
    let used_days = target.div_ceil(mpd);
    let needs_partition = !input.bounds.per_day.is_empty() && used_days >= 2 && mpd >= 2;

    // Canonical recipe order (title, id): deterministic, pool-order independent.
    let canonical = |a: usize, b: usize| {
        input.pool[a]
            .title
            .as_str()
            .cmp(input.pool[b].title.as_str())
            .then(input.pool[a].id.as_str().cmp(input.pool[b].id.as_str()))
    };
    let mut order: Vec<usize> = (0..p).collect();
    order.sort_by(|&a, &b| canonical(a, b));

    // The exact solver is only tractable for a few hundred binaries. Rather than
    // drop a large pool to the nutrition-blind greedy planner, shortlist the
    // recipes that best fit the bounds on their own (for a ratio target, the
    // ones closest to the target split) and solve exactly over those. Re-sorting
    // canonically keeps the model — and solution — pool-order independent.
    let per_recipe_cap = if needs_partition {
        MAX_ILP_INT_VARS / used_days
    } else {
        MAX_ILP_INT_VARS
    };
    if per_recipe_cap < target {
        return None;
    }
    if order.len() > per_recipe_cap {
        order.sort_by(|&a, &b| {
            recipe_fit(input.bounds, &input.macros[a])
                .partial_cmp(&recipe_fit(input.bounds, &input.macros[b]))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| canonical(a, b))
        });
        order.truncate(per_recipe_cap);
        order.sort_by(|&a, &b| canonical(a, b));
    }

    if needs_partition {
        solve_partition(input, &order, target, mpd, used_days)
    } else {
        solve_flat(input, &order, target, used_days)
    }
}

/// How poorly a single recipe fits the bounds treated as one meal / day / plan.
/// Used to shortlist candidates when the pool is too large for the exact solver;
/// lower is better (0 = fits every configured scope on its own).
fn recipe_fit(bounds: &NutritionBounds, m: &Macros) -> f64 {
    recipe_violation(&bounds.per_meal, m)
        + recipe_violation(&bounds.per_day, m)
        + recipe_violation(&bounds.plan, m)
}

/// Non-partition model: one binary per recipe. Per-day bounds either collapse to
/// a per-recipe constant (`mpd == 1`, each recipe is its own day) or apply to the
/// whole selection (`used_days == 1`, a single day).
fn solve_flat(
    input: &GreedyInput<'_>,
    order: &[usize],
    target: usize,
    used_days: usize,
) -> Option<Vec<usize>> {
    let bounds = input.bounds;
    let macros = input.macros;
    let p = order.len();
    let single_day = used_days == 1;
    let fold_per_day = !bounds.per_day.is_empty() && !single_day; // implies mpd == 1

    // Per-recipe violation constant: per-meal always; per-day when each recipe
    // occupies its own day.
    let const_r: Vec<f64> = order
        .iter()
        .map(|&r| {
            let mut v = recipe_violation(&bounds.per_meal, &macros[r]);
            if fold_per_day {
                v += recipe_violation(&bounds.per_day, &macros[r]);
            }
            v
        })
        .collect();

    let build = |phase: Phase, vstar: Option<f64>| -> (Problem, Vec<Variable>) {
        let two = matches!(phase, Phase::Two);
        let mut prob = Problem::new(OptimizationDirection::Minimize);
        let xs: Vec<Variable> = (0..p)
            .map(|j| prob.add_binary_var(if two { 0.0 } else { const_r[j] }))
            .collect();

        let mut viol: Vec<(Variable, f64)> = Vec::new();
        for (j, &c) in const_r.iter().enumerate() {
            if c != 0.0 {
                viol.push((xs[j], c));
            }
        }

        // Exactly `target` recipes selected.
        let count: Vec<(Variable, f64)> = xs.iter().map(|&v| (v, 1.0)).collect();
        prob.add_constraint(&count, ComparisonOp::Eq, target as f64);

        // Slack scopes on whole-selection totals: plan always, per-day when the
        // whole selection is a single day.
        let slack_obj = if two { 0.0 } else { 1.0 };
        for i in 0..4 {
            let plan_range = nutrient_range(&bounds.plan, i);
            let day_range = nutrient_range(&bounds.per_day, i);
            let need_plan = !bounds.plan.is_empty() && !plan_range.is_empty();
            let need_day = single_day && !bounds.per_day.is_empty() && !day_range.is_empty();
            if !need_plan && !need_day {
                continue;
            }
            let terms: Vec<(Variable, f64)> = (0..p)
                .map(|j| (xs[j], nutrient_val(&macros[order[j]], i)))
                .collect();
            if need_plan {
                add_range_slacks(
                    &mut prob,
                    &terms,
                    &plan_range,
                    slack_obj,
                    range_weight(i),
                    &mut viol,
                );
            }
            if need_day {
                add_range_slacks(
                    &mut prob,
                    &terms,
                    &day_range,
                    slack_obj,
                    range_weight(i),
                    &mut viol,
                );
            }
        }

        // Ratio targets on whole-selection totals (plan always; per-day when the
        // whole selection is a single day).
        let need_plan_ratio = !bounds.plan.ratio.is_empty();
        let need_day_ratio = single_day && !bounds.per_day.ratio.is_empty();
        if need_plan_ratio || need_day_ratio {
            let macro_terms: Vec<(Variable, f64, f64, f64)> = (0..p)
                .map(|j| {
                    let m = &macros[order[j]];
                    (xs[j], m.protein_g, m.fat_g, m.carbs_g)
                })
                .collect();
            if need_plan_ratio {
                add_ratio_slacks(
                    &mut prob,
                    &macro_terms,
                    &bounds.plan.ratio,
                    slack_obj,
                    &mut viol,
                );
            }
            if need_day_ratio {
                add_ratio_slacks(
                    &mut prob,
                    &macro_terms,
                    &bounds.per_day.ratio,
                    slack_obj,
                    &mut viol,
                );
            }
        }

        if two {
            // Each recipe maps to a single selection variable.
            let cell_refs: Vec<&[Variable]> = xs.iter().map(std::slice::from_ref).collect();
            build_union(&mut prob, input, order, &cell_refs);
            if let Some(vstar) = vstar {
                prob.add_constraint(&viol, ComparisonOp::Le, vstar + VIOLATION_EPS);
            }
        }
        (prob, xs)
    };

    let (prob1, _) = build(Phase::One, None);
    let vstar = solve_checked(prob1)?.objective();
    let (prob2, xs2) = build(Phase::Two, Some(vstar));
    let sol2 = solve_checked(prob2)?;

    let selected: Vec<usize> = (0..p)
        .filter(|&j| sol2[xs2[j]] > 0.5)
        .map(|j| order[j])
        .collect();
    if selected.len() != target {
        return None;
    }
    Some(selected)
}

/// Partition model: one binary per `(recipe, day)` cell. Used only when a per-day
/// bound genuinely couples multiple meals within a day (`mpd >= 2`, `>= 2` days).
fn solve_partition(
    input: &GreedyInput<'_>,
    order: &[usize],
    target: usize,
    mpd: usize,
    used_days: usize,
) -> Option<Vec<usize>> {
    let bounds = input.bounds;
    let macros = input.macros;
    let p = order.len();
    let cap = |d: usize| mpd.min(target - d * mpd);

    // Per-recipe (per-meal) violation constant; per-day handled by day buckets.
    let const_r: Vec<f64> = order
        .iter()
        .map(|&r| recipe_violation(&bounds.per_meal, &macros[r]))
        .collect();

    let build = |phase: Phase, vstar: Option<f64>| -> (Problem, Vec<Vec<Variable>>) {
        let two = matches!(phase, Phase::Two);
        let mut prob = Problem::new(OptimizationDirection::Minimize);
        // x[j][d]
        let xs: Vec<Vec<Variable>> = (0..p)
            .map(|j| {
                (0..used_days)
                    .map(|_| prob.add_binary_var(if two { 0.0 } else { const_r[j] }))
                    .collect()
            })
            .collect();

        let mut viol: Vec<(Variable, f64)> = Vec::new();
        for (j, cells) in xs.iter().enumerate() {
            if const_r[j] != 0.0 {
                for &v in cells {
                    viol.push((v, const_r[j]));
                }
            }
        }

        // Each recipe used at most once.
        for cells in &xs {
            let terms: Vec<(Variable, f64)> = cells.iter().map(|&v| (v, 1.0)).collect();
            prob.add_constraint(&terms, ComparisonOp::Le, 1.0);
        }
        // Each day holds exactly cap_d recipes.
        let mut day_counts: Vec<Vec<(Variable, f64)>> = vec![Vec::new(); used_days];
        for cells in &xs {
            for (d, &v) in cells.iter().enumerate() {
                day_counts[d].push((v, 1.0));
            }
        }
        for (d, terms) in day_counts.iter().enumerate() {
            prob.add_constraint(terms, ComparisonOp::Eq, cap(d) as f64);
        }

        let slack_obj = if two { 0.0 } else { 1.0 };
        for i in 0..4 {
            // Per-day slacks per day bucket.
            let day_range = nutrient_range(&bounds.per_day, i);
            if !bounds.per_day.is_empty() && !day_range.is_empty() {
                let mut day_terms: Vec<Vec<(Variable, f64)>> = vec![Vec::new(); used_days];
                for (j, cells) in xs.iter().enumerate() {
                    let val = nutrient_val(&macros[order[j]], i);
                    for (d, &v) in cells.iter().enumerate() {
                        day_terms[d].push((v, val));
                    }
                }
                for terms in &day_terms {
                    add_range_slacks(
                        &mut prob,
                        terms,
                        &day_range,
                        slack_obj,
                        range_weight(i),
                        &mut viol,
                    );
                }
            }
            // Plan slacks on the total over all cells.
            let plan_range = nutrient_range(&bounds.plan, i);
            if !bounds.plan.is_empty() && !plan_range.is_empty() {
                let mut terms: Vec<(Variable, f64)> = Vec::with_capacity(p * used_days);
                for (j, cells) in xs.iter().enumerate() {
                    let val = nutrient_val(&macros[order[j]], i);
                    for &v in cells {
                        terms.push((v, val));
                    }
                }
                add_range_slacks(
                    &mut prob,
                    &terms,
                    &plan_range,
                    slack_obj,
                    range_weight(i),
                    &mut viol,
                );
            }
        }

        // Per-day ratio targets per day bucket.
        if !bounds.per_day.ratio.is_empty() {
            let mut buckets: Vec<Vec<(Variable, f64, f64, f64)>> = vec![Vec::new(); used_days];
            for (j, cells) in xs.iter().enumerate() {
                let m = &macros[order[j]];
                for (d, &v) in cells.iter().enumerate() {
                    buckets[d].push((v, m.protein_g, m.fat_g, m.carbs_g));
                }
            }
            for bucket in &buckets {
                add_ratio_slacks(
                    &mut prob,
                    bucket,
                    &bounds.per_day.ratio,
                    slack_obj,
                    &mut viol,
                );
            }
        }
        // Plan ratio target over all cells.
        if !bounds.plan.ratio.is_empty() {
            let mut all: Vec<(Variable, f64, f64, f64)> = Vec::with_capacity(p * used_days);
            for (j, cells) in xs.iter().enumerate() {
                let m = &macros[order[j]];
                for &v in cells {
                    all.push((v, m.protein_g, m.fat_g, m.carbs_g));
                }
            }
            add_ratio_slacks(&mut prob, &all, &bounds.plan.ratio, slack_obj, &mut viol);
        }

        if two {
            let cell_refs: Vec<&[Variable]> = xs.iter().map(|c| c.as_slice()).collect();
            build_union(&mut prob, input, order, &cell_refs);
            if let Some(vstar) = vstar {
                prob.add_constraint(&viol, ComparisonOp::Le, vstar + VIOLATION_EPS);
            }
        }
        (prob, xs)
    };

    let (prob1, _) = build(Phase::One, None);
    let vstar = solve_checked(prob1)?.objective();
    let (prob2, xs2) = build(Phase::Two, Some(vstar));
    let sol2 = solve_checked(prob2)?;

    // Collect each day's selected recipes (canonical order within a day, since
    // recipes are visited in canonical index order).
    let mut days: Vec<Vec<usize>> = vec![Vec::new(); used_days];
    for (j, cells) in xs2.iter().enumerate() {
        for (d, &v) in cells.iter().enumerate() {
            if sol2[v] > 0.5 {
                days[d].push(j); // canonical index
            }
        }
    }
    for (d, group) in days.iter().enumerate() {
        if group.len() != cap(d) {
            return None;
        }
    }

    // Deterministic day labelling: order full days by their contents, keep the
    // short (partial) day last so row-major packing reproduces the grouping.
    // Per-day bounds are identical across days, so relabelling is violation-safe.
    let (mut full, partial): (Vec<Vec<usize>>, Vec<Vec<usize>>) =
        days.into_iter().partition(|g| g.len() == mpd);
    full.sort();
    let mut plan_order: Vec<usize> = Vec::with_capacity(target);
    for group in full.into_iter().chain(partial) {
        for j in group {
            plan_order.push(order[j]);
        }
    }
    if plan_order.len() != target {
        return None;
    }
    Some(plan_order)
}

/// Add one continuous `y[k] ∈ [0,1]` per non-pantry ingredient key (objective
/// coefficient 1) with `y[k] >= x` for every selection cell whose recipe uses
/// `k`. Minimizing `Σ y` equals the net (non-pantry) ingredient union.
/// `cells[j]` is the set of selection variables for canonical recipe `j`.
fn build_union(
    prob: &mut Problem,
    input: &GreedyInput<'_>,
    order: &[usize],
    cells: &[&[Variable]],
) -> Vec<Variable> {
    let mut key_to_cells: HashMap<&crate::domain::IngredientKey, Vec<Variable>> = HashMap::new();
    for (j, &r) in order.iter().enumerate() {
        for k in &input.keys[r] {
            // Treat a key with any pantry stock as covered (set-level proxy for
            // the quantity-aware net_shortfall_count used elsewhere; exact when
            // pantry is empty). This is only the secondary union tiebreak.
            if pantry_quantity_for(k, input.pantry) > super::EPS {
                continue;
            }
            key_to_cells
                .entry(k)
                .or_default()
                .extend(cells[j].iter().copied());
        }
    }
    // Deterministic key order so the model is stable.
    let mut entries: Vec<(&crate::domain::IngredientKey, Vec<Variable>)> =
        key_to_cells.into_iter().collect();
    entries.sort_by(|a, b| {
        a.0.name
            .cmp(&b.0.name)
            .then(kind_rank(a.0.kind).cmp(&kind_rank(b.0.kind)))
    });

    let mut ys = Vec::with_capacity(entries.len());
    for (_key, cell_vars) in entries {
        let y = prob.add_var(1.0, (0.0, 1.0));
        for v in cell_vars {
            prob.add_constraint([(y, 1.0), (v, -1.0)], ComparisonOp::Ge, 0.0);
        }
        ys.push(y);
    }
    ys
}
