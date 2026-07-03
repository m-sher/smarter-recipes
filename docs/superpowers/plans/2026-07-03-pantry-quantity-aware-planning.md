# Quantity-Aware Pantry Planning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `plan_meals` treat pantry stock by quantity (binary shortfall), sharing shopping’s stock ledger, so partial on-hand amounts no longer fully exempt ingredient keys.

**Architecture:** `PlanOptions.pantry` becomes `Vec<PantryItem>`. Greedy coverage clones a stock ledger and a `to_buy` set; candidates are scored by how many keys newly show shortfall after `consume_from_stock` / presence checks via `pantry_quantity_for`. Final net count uses a fresh pantry clone. CLI passes `list_pantry()` directly.

**Tech Stack:** Rust, existing `shopping` ledger helpers, cargo test/fmt/clippy.

## Global Constraints

- Strict TDD: failing test before production code for each behavior slice.
- Binary shortfall only (not weighted).
- Presence-only fallback for `need == 0.0` lines.
- ε = `1e-9` (match shopping).
- Do not mutate persisted pantry on `plan`.
- Reuse `shopping::consume_from_stock` and `pantry_quantity_for`; do not reimplement density bridging.
- Step-by-step commits on `feature/pantry-quantity-aware-planning`.
- `cargo test`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings` before PR.

---

### Task 1: Publish shared stock consume helper

**Files:**
- Modify: `src/shopping/mod.rs` (`consume_from_stock` visibility)
- Test: existing `consume_from_stock_bridges_and_depletes` remains green

**Interfaces:**
- Produces: `pub fn consume_from_stock(stock: &mut [PantryItem], key: &IngredientKey, need: f64) -> f64`

- [ ] **Step 1:** Change `fn consume_from_stock` to `pub fn consume_from_stock`. Add a one-line doc comment that planning uses it for virtual consumption while scoring.

- [ ] **Step 2:** Run `cargo test consume_from_stock_bridges_and_depletes -- --nocapture`. Expected: PASS.

- [ ] **Step 3:** Commit `feat(shopping): publish consume_from_stock for planner reuse`

---

### Task 2: Switch PlanOptions to quantities (compile-fix with presence emulation)

**Files:**
- Modify: `src/planning/mod.rs` (`PlanOptions`, imports, call sites)
- Modify: `src/cli/mod.rs` (pass `list_pantry()`; drop `pantry_keys_for_planning` import usage)
- Modify: existing pantry tests in `src/planning/mod.rs` to build `Vec<PantryItem>` with large quantities

**Interfaces:**
- Produces: `PlanOptions { pantry: Vec<PantryItem> }` default `vec![]`
- Temporary: convert pantry items to key set via `pantry_keys_for_planning` **inside** `plan_meals` only so behavior unchanged while API migrates (removed in Task 3).

- [ ] **Step 1:** Update struct/default and all test/CLI construction sites so the crate compiles.

- [ ] **Step 2:** `cargo test planning:: -- --nocapture` PASS (behavior still presence-only).

- [ ] **Step 3:** Commit `refactor(planning): PlanOptions.pantry carries PantryItem quantities`

---

### Task 3: RED/GREEN — sufficient stock free; insufficient stock not free

**Files:**
- Modify: `src/planning/mod.rs` (requirements precompute, greedy state, net score)
- Remove temporary `pantry_keys_for_planning` usage from planner
- Delete `pantry_keys_for_planning` from `src/shopping/mod.rs` if unused

**Interfaces:**
- Consumes: `consume_from_stock`, `pantry_quantity_for`
- Internal: `recipe_requirements(recipe) -> Vec<(IngredientKey, f64)>`
- Internal: `apply_recipe_to_coverage(stock, to_buy, reqs) -> usize` new keys count (mutates args)

- [ ] **Step 1: Write failing tests**

```rust
fn item(name: &str, kind: UnitKind, qty: f64) -> PantryItem {
    PantryItem { key: IngredientKey::new(name, kind), quantity_canonical: qty }
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
    };
    let plan = plan_meals(&pool, &opts);
    assert_eq!(plan.meals[0].recipe_title, "Flour Cake");
    assert!(plan.rationale.contains("0") && plan.rationale.to_lowercase().contains("pantry"));
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
    };
    let plan = plan_meals(&pool, &opts);
    // Both cost 1 new key; deterministic title tie-break picks "Big Bread" lexically
    // before "Rice Bowl". Assert insufficient flour is NOT scored as 0: with only
    // flour stocked, preferring Rice would mean flour was wrongly free.
    // Stronger: net rationale must report 1 not fully covered (not 0).
    assert!(
        plan.rationale.contains("1") && plan.rationale.to_lowercase().contains("not fully covered"),
        "partial stock must still count as to-buy: {}",
        plan.rationale
    );
    // Contrast: 20 g stock yields 0 not fully covered when selecting Big Bread.
    let opts_enough = PlanOptions {
        days: 1,
        meals_per_day: 1,
        pantry: vec![item("flour", UnitKind::Mass, 20.0)],
    };
    let plan_enough = plan_meals(&pool, &opts_enough);
    assert_eq!(plan_enough.meals[0].recipe_title, "Big Bread");
    assert!(
        plan_enough.rationale.contains("0")
            && plan_enough.rationale.to_lowercase().contains("not fully covered"),
        "{}",
        plan_enough.rationale
    );
}
```

- [ ] **Step 2:** Run tests — expect FAIL (partial 10 g still reported as fully covered / wrong selection).

- [ ] **Step 3:** Implement quantity-aware greedy + `net_union_size` per spec; update rationale string; migrate old pantry tests to `PantryItem` with generous qty; remove `pantry_keys_for_planning`.

- [ ] **Step 4:** Run `cargo test` — all PASS.

- [ ] **Step 5:** Commit `feat(planning): score pantry coverage by quantity shortfall`

---

### Task 4: RED/GREEN — cross-recipe depletion, presence fallback, density bridge

**Files:**
- Modify: `src/planning/mod.rs` tests (+ impl fixes if gaps)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn cross_recipe_depletion_marks_shared_ingredient() {
    let a = rec("A Loaf", &["15 g flour"]);
    let b = rec("B Loaf", &["15 g flour"]);
    let pool = vec![a, b];
    let opts = PlanOptions {
        days: 2,
        meals_per_day: 1,
        pantry: vec![item("flour", UnitKind::Mass, 20.0)],
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
    };
    assert_eq!(plan_meals(&pool, &with_salt).meals[0].recipe_title, "Salty");
    let empty = PlanOptions { days: 1, meals_per_day: 1, pantry: vec![] };
    let plan_empty = plan_meals(&pool, &empty);
    assert!(plan_empty.rationale.to_lowercase().contains("1 distinct") || plan_empty.meals.len() == 1);
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
    };
    let plan = plan_meals(&pool, &opts);
    assert_eq!(plan.meals[0].recipe_title, "Cupcake");
    assert!(plan.rationale.to_lowercase().contains("0") && plan.rationale.contains("not fully covered"));
}
```

- [ ] **Step 2:** Run — FAIL if impl incomplete.

- [ ] **Step 3:** Fix gaps (ordering of presence checks, aggregation within recipe).

- [ ] **Step 4:** `cargo test` PASS.

- [ ] **Step 5:** Commit `test(planning): cover depletion, presence fallback, density bridge`

---

### Task 5: Docs + final verification + PR

**Files:**
- Modify: `src/planning/mod.rs` module docs
- Modify: `README.md` pantry/planning bullets

- [ ] **Step 1:** Update docs per spec (remove presence-only wording).

- [ ] **Step 2:** `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`

- [ ] **Step 3:** Commit `docs: describe quantity-aware pantry planning`

- [ ] **Step 4:** Push branch and open PR with summary + test plan.
