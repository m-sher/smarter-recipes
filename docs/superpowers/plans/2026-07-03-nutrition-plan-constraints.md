# Nutrition Plan Constraints Implementation Plan

> **For agentic workers:** Execute task-by-task with TDD. Steps use checkbox syntax.

**Goal:** Steer `plan` with optional macro min/max ranges from TOML and per-day CLI flags.

**Architecture:** `NutritionBounds` types + violation math in `planning/nutrition_bounds.rs`; nutrition-aware multi-start greedy in `planning/mod.rs`; CLI loads estimates, merges config, prints constraint results.

**Tech Stack:** Rust, serde/toml, clap, existing `Macros` / `recipe_nutrition`.

## Global Constraints

- TDD: failing test before production code for each behavior.
- Commit after each green task.
- No published nutrition in constraint math.
- Empty bounds ⇒ identical planner behavior.

---

### Task 1: Bounds types, validation, TOML parse, CLI merge

**Files:**
- Create: `src/planning/nutrition_bounds.rs`
- Modify: `src/planning/mod.rs` (mod + re-exports)

- [ ] RED: tests for empty bounds, `min > max` err, TOML round-trip, CLI overlay on `per_day`
- [ ] GREEN: implement types + `from_toml_str` + `merge_cli_per_day` + `validate`
- [ ] Commit: `feat(planning): add nutrition bounds config types`

### Task 2: Violation evaluation

**Files:**
- Modify: `src/planning/nutrition_bounds.rs`

- [ ] RED: below min / above max / magnitude sum / empty bounds ⇒ no violations
- [ ] GREEN: `evaluate_macros`, `evaluate_plan`, `violation_magnitude`
- [ ] Commit: `feat(planning): evaluate nutrition bound violations`

### Task 3: Nutrition-aware planner selection

**Files:**
- Modify: `src/planning/mod.rs` (`PlanOptions`, `plan_meals`, greedy/ranking)

- [ ] RED: dessert vs protein fixture prefers feasible day; infeasible pool returns warnings in rationale; empty bounds regression
- [ ] GREEN: precomputed macros, slot-aware filter/tie-break, rank by feasibility then magnitude then net union
- [ ] Commit: `feat(planning): constrain meal plans with nutrition bounds`

### Task 4: CLI flags, wiring, output

**Files:**
- Modify: `src/cli/mod.rs`, `README.md`
- Modify: `tests/e2e.rs` (optional library-level config test)

- [ ] RED: invalid CLI merge errors; plan with bounds prints constraint section via library helpers tested in unit tests; e2e with macros map
- [ ] GREEN: wire flags, build `recipe_macros`, print constraints, stderr on violations, README
- [ ] Commit: `feat(cli): nutrition bounds flags and plan output`
- [ ] Final: `cargo test`, fmt, clippy; open PR
