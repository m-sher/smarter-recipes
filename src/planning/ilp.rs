//! Exact integer-program selection for nutrition- and/or time-of-day-constrained
//! meal plans.
//!
//! When bounds are configured **without** time-of-day steering, the selection is
//! solved with a MILP (HiGHS):
//!
//! * **Variables** — one binary `x` per candidate recipe (flat model) or per
//!   `(recipe, day)` cell when a per-day bound must partition meals across days.
//!   Continuous `y[k] ∈ [0,1]` per non-pantry ingredient key, and continuous
//!   slacks that measure how far each scope total sits outside its min/max.
//! * **Objective** — a two-phase lexicographic solve: phase 1 minimizes the
//!   total **weighted** violation magnitude (calories and the macro-split ratio
//!   prioritized ~5×, see [`super::weighted_magnitude`]); phase 2 fixes that
//!   optimum (`V ≤ V* + ε`) and minimizes the net ingredient union `Σ y`. This
//!   matches the ranking contract in [`super::better_scored`] (feasible-first,
//!   then min weighted violation, then min union).
//!
//! When **time-of-day** steering is on, a slot-indexed model is used instead:
//! one binary per `(recipe, slot)` so assignment respects breakfast/lunch/dinner
//! identity. A three-phase lex solve minimizes nutrition violation, then TOD
//! miss count, then net union.
//!
//! The violation objective reproduces [`super::weighted_magnitude`] exactly:
//! per-meal bounds are a per-recipe constant, per-day/plan bounds are slacks on
//! the relevant totals (each scaled by its constraint weight). Only `x` is
//! integer; `y` and slacks are integral at the optimum on their own.
//!
//! Returns `None` (caller falls back to the greedy planner) when the exact model
//! yields nothing usable — the problem is infeasible, or the time budget elapsed
//! before any feasible plan was found. Otherwise returns the proven optimum, or
//! the best feasible plan found within the budget.
//!
//! Multi-day **per-day** bounds on large pools use a sequential day-pack path
//! ([`solve_days_sequential`]): each day is a small flat MIP (proven-optimal on
//! household pools). A single joint partition MIP over `pool × days` binaries is
//! only attempted when the cell count is small enough to finish within the time
//! budget — otherwise a timed-out incumbent can look “exact” while badly
//! violating soft day caps (e.g. one day at 2× the kcal max).

use std::collections::HashMap;

use backend::{ComparisonOp, OptimizationDirection, Problem, Solution, Variable};

use super::{
    evaluate_macros, slot_requirement, tod_fits, weighted_magnitude, BoundScope, GreedyInput,
    MacroBounds, MacroRange, MacroRatio, KCAL_WEIGHT, MACRO_WEIGHT, RATIO_WEIGHT,
};
use crate::domain::{Macros, UnitKind};
use crate::shopping::pantry_quantity_for;

/// Base wall-clock backstop per solve phase on small models.
const SOLVE_TIME_LIMIT_BASE_SECS: f64 = 5.0;
/// Hard cap per solve phase (seconds).
const SOLVE_TIME_LIMIT_MAX_SECS: f64 = 45.0;
/// Joint `(recipe, day)` partition MIP is only attempted at or below this many
/// binary cells. Above it, sequential day packing is the reliable path.
const PARTITION_CELL_LIMIT: usize = 2_500;
/// Slack allowed above the phase-1 optimum when minimizing the union in phase 2.
const VIOLATION_EPS: f64 = 1e-6;

/// Scale the per-phase time limit with problem size (binary columns).
fn solve_time_secs(num_binaries: usize) -> f64 {
    // ~2ms per binary as a soft guide, floored at the base and capped.
    let scaled = SOLVE_TIME_LIMIT_BASE_SECS + (num_binaries as f64) * 0.002;
    scaled.clamp(SOLVE_TIME_LIMIT_BASE_SECS, SOLVE_TIME_LIMIT_MAX_SECS)
}

/// Thin adapter over the HiGHS MILP solver presenting the small building-block
/// API the models below use (binary/continuous columns, linear rows, a solve
/// that succeeds whenever a feasible primal is available).
mod backend {
    use std::borrow::Borrow;
    use std::ops::Index;

    use highs::{Col, HighsSolutionStatus, RowProblem, Sense};

    /// Objective direction (minimize only).
    pub enum OptimizationDirection {
        Minimize,
    }

    /// A linear constraint's comparison against its right-hand side.
    #[derive(Clone, Copy)]
    pub enum ComparisonOp {
        Eq,
        Le,
        Ge,
    }

    /// A decision-variable handle (a HiGHS column).
    pub type Variable = Col;

    /// A model under construction.
    pub struct Problem {
        inner: RowProblem,
    }

    impl Problem {
        pub fn new(_direction: OptimizationDirection) -> Self {
            Problem {
                inner: RowProblem::default(),
            }
        }

        /// Add a binary (0/1 integer) variable with the given objective coefficient.
        pub fn add_binary_var(&mut self, objective: f64) -> Variable {
            self.inner.add_integer_column(objective, 0..=1)
        }

        /// Add a continuous variable bounded to `[lo, hi]` with the given
        /// objective coefficient.
        pub fn add_var(&mut self, objective: f64, (lo, hi): (f64, f64)) -> Variable {
            self.inner.add_column(objective, lo..=hi)
        }

        /// Add the linear constraint `Σ coef·var  {=,≤,≥}  rhs`.
        pub fn add_constraint<I>(&mut self, terms: I, op: ComparisonOp, rhs: f64)
        where
            I: IntoIterator,
            I::Item: Borrow<(Variable, f64)>,
        {
            let factors = terms.into_iter().map(|t| *t.borrow());
            match op {
                ComparisonOp::Eq => self.inner.add_row(rhs..=rhs, factors),
                ComparisonOp::Le => self.inner.add_row(f64::NEG_INFINITY..=rhs, factors),
                ComparisonOp::Ge => self.inner.add_row(rhs..=f64::INFINITY, factors),
            }
        }

        /// Solve within `time_limit_secs`. Returns a [`Solution`] whenever the
        /// solver has a feasible primal — a proven optimum, or the best incumbent
        /// found so far if it hit the time limit first. Only genuine infeasibility
        /// (or no incumbent yet) yields `None`.
        ///
        /// Uses the default MIP gap (1e-4) and a fixed random seed.
        pub fn solve(self, time_limit_secs: f64) -> Option<Solution> {
            let mut model = self.inner.optimise(Sense::Minimise);
            model.make_quiet();
            model.set_option("time_limit", time_limit_secs);
            model.set_option("random_seed", 0);
            let solved = model.solve();
            if solved.primal_solution_status() != HighsSolutionStatus::Feasible {
                return None;
            }
            Some(Solution {
                objective: solved.objective_value(),
                columns: solved.get_solution().columns().to_vec(),
            })
        }
    }

    /// A solved model's result — the objective value plus each variable's value,
    /// indexable by its [`Variable`] handle. Optimal when the solve proved it,
    /// otherwise the best feasible incumbent found within the time budget.
    pub struct Solution {
        objective: f64,
        columns: Vec<f64>,
    }

    impl Solution {
        pub fn objective(&self) -> f64 {
            self.objective
        }
    }

    impl Index<Variable> for Solution {
        type Output = f64;
        fn index(&self, var: Variable) -> &f64 {
            &self.columns[var.index()]
        }
    }
}

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
/// outside `bounds` — a per-recipe constant matching [`super::weighted_magnitude`].
/// Scope label is irrelevant to the magnitude.
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
/// phase-2 cap.
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
/// beyond the band.
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
        // Weight the slack like the scorer.
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

fn solve_checked(prob: Problem, num_binaries: usize) -> Option<Solution> {
    prob.solve(solve_time_secs(num_binaries))
}

/// Canonical (title, id) order of every pool index.
fn canonical_order(input: &GreedyInput<'_>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..input.pool.len()).collect();
    order.sort_by(|&a, &b| {
        input.pool[a]
            .title
            .as_str()
            .cmp(input.pool[b].title.as_str())
            .then(input.pool[a].id.as_str().cmp(input.pool[b].id.as_str()))
    });
    order
}

/// Exactly select `target` recipes from `input.pool` under configured nutrition
/// bounds and/or time-of-day steering (feasible-first, then min TOD misses, then
/// min union), returning pool indices in plan (row-major) order. `None` means
/// the caller should fall back to greedy.
///
/// For multi-day **per-day** bounds this may return a joint partition MIP on
/// small pools, or [`None`] so the caller can prefer [`solve_days_sequential`]
/// (which scales). TOD still uses the slot model.
pub(super) fn solve_constrained(input: &GreedyInput<'_>, target: usize) -> Option<Vec<usize>> {
    let p = input.pool.len();
    // Nothing to solve, or more meals requested than distinct recipes exist.
    if target == 0 || p == 0 || target > p {
        return None;
    }
    let mpd = input.meals_per_day.max(1) as usize;
    let used_days = target.div_ceil(mpd);
    let order = canonical_order(input);

    if input.time_of_day {
        return solve_slots(input, &order, target, mpd);
    }

    let needs_partition = !input.bounds.per_day.is_empty() && used_days >= 2 && mpd >= 2;
    if needs_partition {
        // Large joint models time out with poor soft-constraint incumbents.
        // Leave them to sequential day packing (caller).
        if p.saturating_mul(used_days) > PARTITION_CELL_LIMIT {
            return None;
        }
        solve_partition(input, &order, target, mpd, used_days)
    } else {
        solve_flat(input, &order, target, used_days, true)
    }
}

/// Pack each day with its own flat exact MIP on the **remaining** pool.
///
/// Each day uses `used_days = 1` so per-day min/max/ratio apply to that day's
/// meals alone (the path that already works for single-day plans). Recipes are
/// removed after each day so the plan never repeats. Scales as
/// `O(days × flat(pool))` instead of one `pool × days` joint MIP.
///
/// Nutrition feasibility is preferred over a globally minimal shopping list:
/// each day runs a **nutrition-only** flat MIP (no union phase) so multi-day
/// plans on multi-thousand pools stay within memory/time budgets.
pub(super) fn solve_days_sequential(input: &GreedyInput<'_>, target: usize) -> Option<Vec<usize>> {
    let p = input.pool.len();
    if target == 0 || p == 0 || target > p {
        return None;
    }
    let mpd = input.meals_per_day.max(1) as usize;
    let used_days = target.div_ceil(mpd);
    let mut remaining = canonical_order(input);
    // Recipes that alone exceed the day kcal max can never join a zero-violation
    // day; drop them from the sequential pool (kept only if that would leave too
    // few candidates to fill the plan — best-effort fallback).
    if let Some(day_max) = input.bounds.per_day.kcal.max {
        let filtered: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&ri| input.macros[ri].kcal <= day_max)
            .collect();
        if filtered.len() >= target {
            remaining = filtered;
        }
    }
    let mut plan: Vec<usize> = Vec::with_capacity(target);

    for _ in 0..used_days {
        let cap = mpd.min(target - plan.len());
        if cap == 0 {
            break;
        }
        if remaining.len() < cap {
            return None;
        }
        // Flat single-day nutrition MIP (no union phase — keeps large pools safe).
        let day = solve_flat(input, &remaining, cap, 1, false)?;
        if day.len() != cap {
            return None;
        }
        // Deterministic within-day order (title, id).
        let mut day = day;
        day.sort_by(|&a, &b| {
            input.pool[a]
                .title
                .as_str()
                .cmp(input.pool[b].title.as_str())
                .then(input.pool[a].id.as_str().cmp(input.pool[b].id.as_str()))
        });
        for &ri in &day {
            remaining.retain(|&x| x != ri);
        }
        plan.extend(day);
    }

    if plan.len() != target {
        return None;
    }
    Some(plan)
}

/// Non-partition model: one binary per recipe. Per-day bounds either collapse to
/// a per-recipe constant (`mpd == 1`, each recipe is its own day) or apply to the
/// whole selection (`used_days == 1`, a single day).
///
/// When `with_union` is false, only the nutrition-violation phase runs (used by
/// sequential multi-day packing on large pools to avoid the heavy ingredient-union
/// MIP that can exhaust memory).
fn solve_flat(
    input: &GreedyInput<'_>,
    order: &[usize],
    target: usize,
    used_days: usize,
    with_union: bool,
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

    let (prob1, xs1) = build(Phase::One, None);
    let sol1 = solve_checked(prob1, p)?;
    if !with_union {
        let selected: Vec<usize> = (0..p)
            .filter(|&j| sol1[xs1[j]] > 0.5)
            .map(|j| order[j])
            .collect();
        return (selected.len() == target).then_some(selected);
    }

    let vstar = sol1.objective();
    let (prob2, xs2) = build(Phase::Two, Some(vstar));
    let sol2 = solve_checked(prob2, p)?;

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
/// bound couples multiple meals within a day (`mpd >= 2`, `>= 2` days).
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

    let n_binaries = p.saturating_mul(used_days);
    let (prob1, _) = build(Phase::One, None);
    let vstar = solve_checked(prob1, n_binaries)?.objective();
    let (prob2, xs2) = build(Phase::Two, Some(vstar));
    let sol2 = solve_checked(prob2, n_binaries)?;

    // Collect each day's selected recipes (canonical order within a day).
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
    // short (partial) day last.
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
            // Treat a key with any pantry stock as covered.
            if pantry_quantity_for(k, input.pantry) > super::EPS {
                continue;
            }
            key_to_cells
                .entry(k)
                .or_default()
                .extend(cells[j].iter().copied());
        }
    }
    // Deterministic key order.
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

#[derive(Clone, Copy)]
enum SlotPhase {
    /// Minimize total nutrition-violation magnitude.
    Nutrition,
    /// Fix nutrition optimum; minimize TOD miss count.
    Tod,
    /// Fix nutrition + TOD optima; minimize net ingredient union.
    Union,
}

/// Slot-indexed model used when time-of-day steering is on: one binary per
/// `(recipe, slot)` so breakfast/lunch/dinner identity is explicit.
///
/// Cost note: variables scale as `pool × slots` over **3** lex phases (nutrition
/// → TOD misses → union), vs flat `pool` binaries × 2 phases. Large catalogs can
/// hit the solve-time budget more readily and fall back to greedy; no candidate
/// cap is applied here (pool is already the caller's selected set).
fn solve_slots(
    input: &GreedyInput<'_>,
    order: &[usize],
    target: usize,
    mpd: usize,
) -> Option<Vec<usize>> {
    let bounds = input.bounds;
    let macros = input.macros;
    let p = order.len();
    let slots = target;
    let used_days = target.div_ceil(mpd);
    let mpd_u = mpd as u32;

    let const_r: Vec<f64> = order
        .iter()
        .map(|&r| recipe_violation(&bounds.per_meal, &macros[r]))
        .collect();

    let miss: Vec<Vec<bool>> = (0..p)
        .map(|j| {
            let labels = input.tod_labels[order[j]];
            (0..slots)
                .map(|s| {
                    let meal = (s as u32) % mpd_u;
                    let req = slot_requirement(mpd_u, meal);
                    !tod_fits(labels, req)
                })
                .collect()
        })
        .collect();

    type SlotBuild = (
        Problem,
        Vec<Vec<Variable>>,
        Vec<(Variable, f64)>,
        Vec<(Variable, f64)>,
    );
    let build = |phase: SlotPhase, v_nut: Option<f64>, v_tod: Option<f64>| -> SlotBuild {
        let mut prob = Problem::new(OptimizationDirection::Minimize);
        let nutrition_obj = matches!(phase, SlotPhase::Nutrition);
        let tod_obj = matches!(phase, SlotPhase::Tod);
        let union_phase = matches!(phase, SlotPhase::Union);

        let xs: Vec<Vec<Variable>> = (0..p)
            .map(|j| {
                (0..slots)
                    .map(|s| {
                        let mut obj = 0.0;
                        if nutrition_obj {
                            obj += const_r[j];
                        }
                        if tod_obj && miss[j][s] {
                            obj += 1.0;
                        }
                        prob.add_binary_var(obj)
                    })
                    .collect()
            })
            .collect();

        let mut nut_viol: Vec<(Variable, f64)> = Vec::new();
        for (j, cells) in xs.iter().enumerate() {
            if const_r[j] != 0.0 {
                for &v in cells {
                    nut_viol.push((v, const_r[j]));
                }
            }
        }

        let mut tod_viol: Vec<(Variable, f64)> = Vec::new();
        for (j, cells) in xs.iter().enumerate() {
            for (s, &v) in cells.iter().enumerate() {
                if miss[j][s] {
                    tod_viol.push((v, 1.0));
                }
            }
        }

        // Each recipe at most once.
        for cells in &xs {
            let terms: Vec<(Variable, f64)> = cells.iter().map(|&v| (v, 1.0)).collect();
            prob.add_constraint(&terms, ComparisonOp::Le, 1.0);
        }
        // Each slot exactly one recipe.
        #[allow(clippy::needless_range_loop)]
        for s in 0..slots {
            let terms: Vec<(Variable, f64)> = (0..p).map(|j| (xs[j][s], 1.0)).collect();
            prob.add_constraint(&terms, ComparisonOp::Eq, 1.0);
        }

        let slack_obj = if nutrition_obj { 1.0 } else { 0.0 };
        for i in 0..4 {
            let day_range = nutrient_range(&bounds.per_day, i);
            if !bounds.per_day.is_empty() && !day_range.is_empty() {
                let mut day_terms: Vec<Vec<(Variable, f64)>> = vec![Vec::new(); used_days];
                for (j, cells) in xs.iter().enumerate() {
                    let val = nutrient_val(&macros[order[j]], i);
                    for (s, &v) in cells.iter().enumerate() {
                        day_terms[s / mpd].push((v, val));
                    }
                }
                for terms in &day_terms {
                    if terms.is_empty() {
                        continue;
                    }
                    add_range_slacks(
                        &mut prob,
                        terms,
                        &day_range,
                        slack_obj,
                        range_weight(i),
                        &mut nut_viol,
                    );
                }
            }
            let plan_range = nutrient_range(&bounds.plan, i);
            if !bounds.plan.is_empty() && !plan_range.is_empty() {
                let mut terms: Vec<(Variable, f64)> = Vec::with_capacity(p * slots);
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
                    &mut nut_viol,
                );
            }
        }

        if !bounds.per_day.ratio.is_empty() {
            let mut buckets: Vec<Vec<(Variable, f64, f64, f64)>> = vec![Vec::new(); used_days];
            for (j, cells) in xs.iter().enumerate() {
                let m = &macros[order[j]];
                for (s, &v) in cells.iter().enumerate() {
                    buckets[s / mpd].push((v, m.protein_g, m.fat_g, m.carbs_g));
                }
            }
            for bucket in &buckets {
                if bucket.is_empty() {
                    continue;
                }
                add_ratio_slacks(
                    &mut prob,
                    bucket,
                    &bounds.per_day.ratio,
                    slack_obj,
                    &mut nut_viol,
                );
            }
        }
        if !bounds.plan.ratio.is_empty() {
            let mut all: Vec<(Variable, f64, f64, f64)> = Vec::with_capacity(p * slots);
            for (j, cells) in xs.iter().enumerate() {
                let m = &macros[order[j]];
                for &v in cells {
                    all.push((v, m.protein_g, m.fat_g, m.carbs_g));
                }
            }
            add_ratio_slacks(
                &mut prob,
                &all,
                &bounds.plan.ratio,
                slack_obj,
                &mut nut_viol,
            );
        }

        if let Some(vstar) = v_nut {
            if !nut_viol.is_empty() {
                prob.add_constraint(&nut_viol, ComparisonOp::Le, vstar + VIOLATION_EPS);
            }
        }
        if let Some(tstar) = v_tod {
            if !tod_viol.is_empty() {
                prob.add_constraint(&tod_viol, ComparisonOp::Le, tstar + VIOLATION_EPS);
            }
        }

        if union_phase {
            let cell_refs: Vec<&[Variable]> = xs.iter().map(|c| c.as_slice()).collect();
            build_union(&mut prob, input, order, &cell_refs);
        }

        (prob, xs, nut_viol, tod_viol)
    };

    let n_binaries = p.saturating_mul(slots);
    let (prob1, _, _, _) = build(SlotPhase::Nutrition, None, None);
    let v_nut = solve_checked(prob1, n_binaries)?.objective();
    let (prob2, _, _, _) = build(SlotPhase::Tod, Some(v_nut), None);
    let v_tod = solve_checked(prob2, n_binaries)?.objective();
    let (prob3, xs3, _, _) = build(SlotPhase::Union, Some(v_nut), Some(v_tod));
    let sol3 = solve_checked(prob3, n_binaries)?;

    let mut plan_order = vec![0usize; slots];
    let mut filled = vec![false; slots];
    for (j, cells) in xs3.iter().enumerate() {
        for (s, &v) in cells.iter().enumerate() {
            if sol3[v] > 0.5 {
                if filled[s] {
                    return None;
                }
                plan_order[s] = order[j];
                filled[s] = true;
            }
        }
    }
    if filled.iter().any(|f| !f) {
        return None;
    }
    Some(plan_order)
}
