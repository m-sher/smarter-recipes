# Quantity-Aware Pantry Planning — Design

**Date:** 2026-07-03  
**Status:** Approved  
**Branch:** `feature/pantry-quantity-aware-planning`

## Problem

Meal planning treats on-hand pantry stock as a set of ingredient identities
(`HashSet<IngredientKey>`). Any positive quantity fully exempts that key from
the planner’s “need to source” cost. Shopping already subtracts **quantities**
(with mass↔volume density bridging). The split is documented in `README.md`
(“planning: presence-only, not quantity”).

Consequence: if a recipe needs 20 g flour and the pantry holds 10 g, planning
scores flour as free, but `shop` still requires a purchase. Plans are biased
toward recipes that only *mention* stocked ingredients, even when stock is
insufficient.

## Goal

Make `plan_meals` quantity-aware with **binary shortfall** semantics:

- Need 10 g, have 20 g → key does **not** count as to-buy; remaining stock 10 g.
- Need 20 g, have 10 g → key **does** count as to-buy; stock for that identity
  goes to 0 after consumption.
- Across selected meals, consume stock in schedule order so aggregate demand
  vs pantry matches what `apply_pantry_to_requirements` would conclude.
- Ingredient lines with **no parsed quantity** use a **presence-only fallback**:
  covered iff any positive exact or density-bridged stock exists; otherwise
  to-buy. Do not invent amounts. Aligns with `Store::aggregate_ingredients`
  registering `0.0` for presence.
- Empty pantry (or default `PlanOptions`) preserves prior selection behavior.
- Do **not** mutate persisted pantry on `plan`; only `pantry restock` writes
  stock.

Primary objective stays **minimize the number of distinct ingredient keys with
any unmet demand** after quantity-aware consumption. No weighted shortfall
scoring; existing tie-breaks (compact recipe, title, id) unchanged.

## Non-goals

- Changing package optimization or restock semantics.
- Weighted / cost-based planner objectives.
- Depleting SQLite pantry rows when generating a plan.
- UI changes beyond rationale string wording.

## Current architecture (relevant bits)

| Piece | Role today |
| --- | --- |
| `PlanOptions.pantry: HashSet<IngredientKey>` | Presence coverage seed |
| `recipe_keys` / `greedy_from_seed` / `net_union_size` | Set difference on keys |
| `shopping::consume_from_stock` (private) | Exact key then density partner; no double credit |
| `shopping::apply_pantry_to_requirements` | Batch net requirements for shop |
| `shopping::pantry_keys_for_planning` | Expand mass↔volume identities for planner |
| CLI `plan` | `list_pantry` → `pantry_keys_for_planning` → `plan_meals` |

## Design

### Shared stock ledger

Promote the shopping ledger helpers so planning and shopping share one
consumption model:

- `consume_from_stock(stock, key, need) -> shortfall` — `pub` (or `pub(crate)`
  if the crate boundary allows tests in-module only; prefer `pub` for clarity
  next to `apply_pantry_to_requirements` and `pantry_quantity_for`).
- Keep `add_to_stock` as needed for restock; visibility unchanged unless tests
  require it.
- `pantry_quantity_for` remains the read-only availability helper (exact then
  density bridge).

Planning must not reimplement bridging. After this change,
`pantry_keys_for_planning` is unused on the plan path; remove it if nothing
else references it (avoid clippy dead-code), or delete outright.

### API

```rust
pub struct PlanOptions {
    pub days: u32,
    pub meals_per_day: u32,
    /// On-hand stock in canonical units; consumed virtually while scoring.
    pub pantry: Vec<PantryItem>,
}
```

CLI passes `store.list_pantry()?` directly.

### Per-recipe requirements

Replace key-only precomputation used for scoring with aggregated requirements
per recipe:

- `IngredientKey::from_line` + `canonical_quantity()` summed per key.
- Missing quantity → `(key, 0.0)` presence sentinel (insert if absent; do not
  add to an existing positive sum).
- Retain a key set (or derive keys from requirements) for total distinct
  reporting (`plan_union_size`, rationale “Plan uses N distinct ingredient
  key(s)”).

### Greedy coverage state

Running state while building a schedule:

1. `stock: Vec<PantryItem>` — clone of `opts.pantry`.
2. `to_buy: HashSet<IngredientKey>` — keys with any shortfall so far.

**Score a candidate** (clone state; do not mutate the running state until
commit):

For each `(key, need)` in the candidate’s requirements:

- If `key` is already in `to_buy`, skip (0 new cost for this key).
- If `need <= ε` (presence-only): covered if `pantry_quantity_for(key, stock) > ε`;
  else mark as new to-buy. Do not consume stock.
- If `need > ε`: `shortfall = consume_from_stock(&mut stock, key, need)`;
  if `shortfall > ε`, mark as new to-buy.

`new_keys =` count of keys newly marked. Tie-break tuple unchanged:
`(new_keys, requirement_line_count_or_key_len, title, recipe_id)`.

**Commit** the chosen candidate by applying the same consumption to the running
`stock` and unioning new keys into `to_buy`.

Seed handling: start from empty selection logic as today, but seed coverage
from quantity-aware application of the seed recipe (not “all pantry keys are
covered forever”).

### Final net score and rationale

`net_union_size(selected)` = number of keys with shortfall after applying all
selected recipes’ requirements to a **fresh** clone of `opts.pantry` (same
rules as scoring). Equivalent end state to aggregating requirements then
`apply_pantry_to_requirements` and counting remaining lines (presence-only
keys with `need == 0` that lack stock count as one line each).

Rationale copy updates from “not already in pantry” / “pantry key(s)
considered” to quantity-aware wording, e.g. “N not fully covered by pantry
stock; M pantry item(s) considered”.

Multi-start selection still minimizes this net count; lex `(title, id)`
sequence tie-break unchanged via `better_schedule`.

### ε

Use the same tolerance as shopping (`1e-9`) for shortfall and presence checks.

## Testing (TDD)

Strict red → green per behavior; one logical commit per slice.

1. **Insufficient stock is not free** — 10 g flour in pantry; recipe A needs
   20 g flour; recipe B needs an unstocked different single ingredient. Planner
   must not treat A as zero-cost; with one slot, prefer the deterministic
   tie-break among equal costs **or** assert explicitly that A’s net cost is 1
   (helper/unit test on scoring) and that sufficient-stock contrast differs.
2. **Sufficient stock is free** — 20 g flour covers 10 g need (0 to-buy);
   prefer that recipe over one needing an unstocked ingredient.
3. **Cross-recipe depletion** — 20 g flour; two recipes each needing 15 g;
   two slots → net to-buy includes flour; rationale reflects quantity-aware
   net count.
4. **Presence-only fallback** — `"salt to taste"` covered when salt stocked;
   to-buy when absent.
5. **Density bridge parity** — `500g flour` pantry covers volume-measured
   flour in a recipe when a density entry exists.
6. **Empty pantry regression** — existing selection tests pass with
   `pantry: vec![]` / `Default`.
7. **Migrate existing pantry tests** to `Vec<PantryItem>` with generous
   quantities so prior “fully stocked keys are free” examples keep intent.

Update `tests/e2e.rs` only as needed for `PlanOptions` shape (default still
works).

Promote/adjust shopping unit tests only if visibility changes require it;
existing ledger tests remain the contract for `consume_from_stock`.

## Documentation

- `src/planning/mod.rs` module docs: describe quantity-aware binary shortfall
  and shared ledger.
- `README.md` design bullet and planning summary: remove “presence-only, not
  quantity”; describe binary shortfall + density bridging parity with shop.

## Implementation order

1. Expose `consume_from_stock` (fail any visibility-only compile gaps; no
   behavior change).
2. Change `PlanOptions` + fix compile sites (CLI, tests) with **temporary**
   presence emulation **only if needed to compile** — prefer switching
   planner internals in the same TDD slices so we never land presence-only
   emulation as the final behavior.
3. RED/GREEN: sufficient stock free; insufficient not free.
4. RED/GREEN: cross-recipe depletion; presence-only fallback; density bridge.
5. Update rationale strings + docs.
6. `cargo test`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`.
7. Open PR.

## Success criteria

- Partial pantry quantities no longer fully exempt keys from plan cost.
- Planner and shop agree on which keys still need sourcing for a given plan
  and pantry (binary: any shortfall ⇔ shopping list line).
- All tests green; clippy `-D warnings` clean; step-by-step commits on
  `feature/pantry-quantity-aware-planning`.
