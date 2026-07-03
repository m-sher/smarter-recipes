# Nutrition plan constraints

**Status:** Approved  
**Date:** 2026-07-03  
**Branch:** `feature/nutrition-plan-constraints`

## Problem

`plan` minimizes distinct ingredient keys and only reports nutrition afterward. Nothing stops a day of two low-protein desserts. Users need configurable macro ranges that steer selection.

## Goals

- Require optional min/max ranges on `kcal`, `protein_g`, `fat_g`, `carbs_g`.
- Configure via TOML file (all scopes) and/or CLI flags (per-day only).
- Use **estimated** whole-recipe macros (`recipe_nutrition` + cache overlay), not published per-serving data.
- Trust estimates as-is; always report coverage.
- **Hard when possible:** prefer fully feasible plans; if none, return best-effort (least total violation, then min-union) with warnings in rationale and on stderr.
- No behavior change when no bounds are configured.

## Non-goals

- Published/schema.org nutrition in constraint math.
- Soft weighted scoring or user-defined penalty weights.
- Coverage thresholds that disable enforcement.
- Non-macro nutrients, allergens, or meal-type tags.
- Look-ahead pruning beyond rejecting immediate max / per-meal violations.

## Configuration

### TOML (`--nutrition-config PATH`)

```toml
[per_day]
protein_g = { min = 50.0, max = 200.0 }
kcal = { max = 3000.0 }

[per_meal]
protein_g = { min = 15.0 }

[plan]
protein_g = { min = 350.0 }
```

Sections and nutrients are optional. A nutrient entry may set `min`, `max`, or both. `min > max` is a parse/validation error.

### CLI (per-day only)

`--min-protein-g`, `--max-protein-g`, `--min-kcal`, `--max-kcal`, `--min-fat-g`, `--max-fat-g`, `--min-carbs-g`, `--max-carbs-g`, and `--nutrition-config <path>`.

### Merge

1. Start from empty bounds.
2. If `--nutrition-config` is set, load and validate the file.
3. For each CLI flag present, overlay that field onto `per_day`.
4. Re-validate.

## Planner integration

Extend `PlanOptions` with `nutrition: NutritionBounds` and precomputed `recipe_macros: HashMap<RecipeId, Macros>` (missing → zeros).

Multi-start greedy is nutrition-aware when any bound is set:

1. Each append fills the next row-major `(day, meal)` slot.
2. Reject candidates that violate **per-meal** bounds for their macros.
3. Reject candidates that would push the running **per-day** totals above any per-day **max**.
4. Tie-break after fewer new ingredient keys (and compact recipe size): prefer candidates that most reduce remaining per-day **min** deficit for the current day; then title; then id.
5. After each day completes, record per-day min/max violations.
6. After the schedule completes, record plan-total violations.
7. Rank schedules: feasible (zero violations) by net union size; else by total violation magnitude (sum of distances past failing bounds), then net union; then lex `(title, id)` sequence.

When bounds are empty, keep the existing algorithm and tie-breaks unchanged.

Rationale mentions active nutrition constraints and lists violations when present.

## CLI output

Existing per-day estimate block unchanged. When bounds are configured, print a constraints summary: satisfied or each violation (`scope`, nutrient, actual, bound). Stderr warnings only when violations remain. Coverage reporting unchanged.

## Module layout

- `src/planning/nutrition_bounds.rs` — types, TOML load, CLI merge, validation, violation evaluation.
- `src/planning/mod.rs` — constrained greedy + ranking.
- `src/cli/mod.rs` — flags, load macros, print constraints, stderr warnings.
- `README.md` — document flags and example TOML.

## Testing (TDD)

1. Parse/validate/merge bounds.
2. Violation magnitude math on `Macros` totals.
3. Planner: with per-day min protein, prefer a protein meal beside a dessert over two desserts when the pool allows a feasible plan.
4. Planner: when infeasible, still returns a plan; rationale includes violations; violation magnitude beats a worse alternative.
5. Empty bounds: identical selection to prior behavior on existing fixtures.
6. CLI/e2e: `--min-protein-g` and `--nutrition-config` accepted; invalid `min > max` errors.

## Success criteria

- `cargo test`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings` pass.
- Unconstrained `plan` behavior unchanged.
- Constrained plans avoid all-dessert days when a feasible alternative exists in the pool.
