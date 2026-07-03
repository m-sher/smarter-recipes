//! Purchase optimization: choose package sizes covering required amounts.
//!
//! # Algorithm
//!
//! For each ingredient with a required amount `R` (canonical units) and a catalog
//! of packages `(size_i, price_i?)`:
//!
//! 1. Enumerate multisets of packages whose total size `P >= R`, bounding the
//!    search to at most `max_packages` items.
//!
//! 2. Rank feasible combinations by:
//!    - **Primary:** minimum total cost (unknown if any package lacks a price)
//!    - **Secondary:** minimum leftover `P - R`
//!    - **Tertiary:** fewer packages
//!
//! Dry goods measured by volume in recipes are converted to mass via
//! [`crate::pricing::density`] when a density is known, so packages and leftovers
//! reflect real grocery units (lb/oz bags), not fictitious fl-oz flour.
//!
//! # Per-trip / ordering benefit
//!
//! [`trip_breakdown_for_plan`] walks meals in plan order and reports which
//! ingredient keys are **newly introduced** at each meal (not seen in earlier
//! meals). That makes the planner's ordering observable: shared ingredients
//! appear once as "new" on the earliest meal that needs them, then are covered
//! for later meals. A "trip" is modeled as one shopping day; `new_keys_per_day`
//! sums first-seen keys by plan day.

mod optimize;

pub use optimize::{optimize_purchase, optimize_shopping_list, OptimizeOptions};

use crate::domain::{IngredientKey, MealPlan, PantryItem, ShoppingItem, ShoppingList, UnitKind};
use crate::pricing::{mass_g_to_volume_ml, volume_ml_to_mass_g, PackageCatalog};
use crate::storage::Store;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// On-hand amount for `key`, preferring an exact identity match.
///
/// If none exists, bridges mass↔volume via the density table for the same
/// ingredient name (so `pantry add "500g flour"` covers volume-measured flour
/// in recipes). Exact and converted stock are never double-counted.
pub fn pantry_quantity_for(key: &IngredientKey, pantry: &[PantryItem]) -> f64 {
    if let Some(p) = pantry.iter().find(|p| p.key == *key) {
        return p.quantity_canonical;
    }
    match key.kind {
        UnitKind::Volume => pantry
            .iter()
            .find(|p| p.key.name == key.name && p.key.kind == UnitKind::Mass)
            .and_then(|p| mass_g_to_volume_ml(&p.key.name, p.quantity_canonical))
            .unwrap_or(0.0),
        UnitKind::Mass => pantry
            .iter()
            .find(|p| p.key.name == key.name && p.key.kind == UnitKind::Volume)
            .and_then(|p| volume_ml_to_mass_g(&p.key.name, p.quantity_canonical))
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Convert `amount` of `name` from one unit kind to another via the density
/// table; identity for same-kind, `None` when no bridge exists.
fn convert_kind(name: &str, amount: f64, from: UnitKind, to: UnitKind) -> Option<f64> {
    match (from, to) {
        (a, b) if a == b => Some(amount),
        (UnitKind::Mass, UnitKind::Volume) => mass_g_to_volume_ml(name, amount),
        (UnitKind::Volume, UnitKind::Mass) => volume_ml_to_mass_g(name, amount),
        _ => None,
    }
}

/// Consume up to `need` (in `key`'s canonical units) from the mutable `stock`
/// ledger: exact-kind match first, then the density-bridged partner kind.
/// Mutates `stock` (never below zero) so no unit of stock is credited twice.
/// Returns the unmet remainder.
fn consume_from_stock(stock: &mut [PantryItem], key: &IngredientKey, need: f64) -> f64 {
    let mut remaining = need;
    if let Some(item) = stock
        .iter_mut()
        .find(|p| p.key == *key && p.quantity_canonical > 0.0)
    {
        let take = remaining.min(item.quantity_canonical);
        item.quantity_canonical -= take;
        remaining -= take;
    }
    if remaining <= 1e-9 {
        return 0.0;
    }
    let partner = match key.kind {
        UnitKind::Volume => UnitKind::Mass,
        UnitKind::Mass => UnitKind::Volume,
        _ => return remaining,
    };
    if let Some(item) = stock
        .iter_mut()
        .find(|p| p.key.name == key.name && p.key.kind == partner && p.quantity_canonical > 0.0)
    {
        if let Some(avail_in_key) =
            convert_kind(&key.name, item.quantity_canonical, partner, key.kind)
        {
            let take_key = remaining.min(avail_in_key);
            if let Some(take_partner) = convert_kind(&key.name, take_key, key.kind, partner) {
                item.quantity_canonical = (item.quantity_canonical - take_partner).max(0.0);
                remaining -= take_key;
            }
        }
    }
    remaining.max(0.0)
}

/// Add `qty` (in `key`'s canonical units) into the `stock` ledger, merging with
/// an existing exact-key row or appending a new one.
fn add_to_stock(stock: &mut Vec<PantryItem>, key: &IngredientKey, qty: f64) {
    if let Some(item) = stock.iter_mut().find(|p| p.key == *key) {
        item.quantity_canonical += qty;
    } else {
        stock.push(PantryItem {
            key: key.clone(),
            quantity_canonical: qty,
        });
    }
}

/// Subtract on-hand pantry quantities from plan requirements.
///
/// Consumes a working copy of the pantry so a single stock row is never credited
/// against more than one requirement (exact [`IngredientKey`] first, then the
/// mass↔volume density bridge). Items fully covered by the pantry are omitted.
pub fn apply_pantry_to_requirements(
    requirements: &[(IngredientKey, f64)],
    pantry: &[PantryItem],
) -> Vec<(IngredientKey, f64)> {
    let mut stock = pantry.to_vec();
    let mut out = Vec::with_capacity(requirements.len());
    for (key, need) in requirements {
        let shortfall = consume_from_stock(&mut stock, key, *need);
        if shortfall > 1e-9 {
            out.push((key.clone(), shortfall));
        }
    }
    out
}

/// Expand pantry identities for planner coverage: mass stock of a density-known
/// dry good also covers the volume key (and vice versa).
pub fn pantry_keys_for_planning(pantry: &[PantryItem]) -> HashSet<IngredientKey> {
    let mut keys = HashSet::new();
    for item in pantry {
        keys.insert(item.key.clone());
        match item.key.kind {
            UnitKind::Mass if volume_ml_to_mass_g(&item.key.name, 1.0).is_some() => {
                keys.insert(IngredientKey::new(&item.key.name, UnitKind::Volume));
            }
            UnitKind::Volume if volume_ml_to_mass_g(&item.key.name, 1.0).is_some() => {
                keys.insert(IngredientKey::new(&item.key.name, UnitKind::Mass));
            }
            _ => {}
        }
    }
    keys
}

/// Purchases to add and cooked amounts to deduct when completing a plan trip.
///
/// Apply **additions first**, then **deductions**. With an empty pantry the net
/// for each bought ingredient is package leftover (`purchased − required`).
/// Ingredients fully covered by existing stock only appear in `deductions`.
#[derive(Debug, Clone, PartialEq)]
pub struct RestockDelta {
    pub additions: Vec<(IngredientKey, f64)>,
    pub deductions: Vec<(IngredientKey, f64)>,
}

/// Compute restock delta from gross plan requirements and the (already
/// pantry-netted) shopping list for that plan.
pub fn compute_restock_delta(
    gross_requirements: &[(IngredientKey, f64)],
    shopping_list: &ShoppingList,
    catalog: &PackageCatalog,
) -> RestockDelta {
    let additions: Vec<_> = shopping_list
        .items
        .iter()
        .filter_map(|item: &ShoppingItem| {
            let qty = catalog.purchased_to_key_units(&item.ingredient, item.purchased_canonical);
            if qty > 1e-9 {
                Some((item.ingredient.clone(), qty))
            } else {
                None
            }
        })
        .collect();
    let deductions: Vec<_> = gross_requirements
        .iter()
        .filter(|(_, need)| *need > 1e-9)
        .map(|(k, need)| (k.clone(), *need))
        .collect();
    RestockDelta {
        additions,
        deductions,
    }
}

/// Buy packages (add to pantry) then cook the plan (deduct full requirements).
///
/// Idempotent per plan: a second call for the same plan id errors without
/// mutating stock. Net empty-pantry end state is packaging leftover only.
pub fn restock_plan_from_shop(
    store: &Store,
    plan: &MealPlan,
    catalog: &PackageCatalog,
) -> Result<RestockDelta> {
    if store.is_plan_restocked(&plan.id)? {
        bail!(
            "plan {} was already restocked; refusing to double-apply purchases/consumption",
            &plan.id[..plan.id.len().min(8)]
        );
    }
    let ids: Vec<_> = plan.meals.iter().map(|m| m.recipe_id.clone()).collect();
    let gross = store.aggregate_ingredients(&ids)?;
    let list = shopping_list_for_plan(store, plan, catalog)?;
    let delta = compute_restock_delta(&gross, &list, catalog);

    // Build the post-trip pantry ledger in memory (purchases in, then cooked
    // amounts consumed against real stock via the density bridge), then persist
    // every touched row plus the restock mark in one transaction.
    let mut stock = store.list_pantry()?;
    for (key, qty) in &delta.additions {
        add_to_stock(&mut stock, key, *qty);
    }
    for (key, qty) in &delta.deductions {
        consume_from_stock(&mut stock, key, *qty);
    }
    store.apply_restock(&plan.id, &stock)?;
    Ok(delta)
}

/// Build an optimized shopping list for a stored plan, net of pantry stock.
pub fn shopping_list_for_plan(
    store: &Store,
    plan: &MealPlan,
    catalog: &PackageCatalog,
) -> Result<ShoppingList> {
    let ids: Vec<_> = plan.meals.iter().map(|m| m.recipe_id.clone()).collect();
    let requirements = store.aggregate_ingredients(&ids)?;
    let pantry = store.list_pantry()?;
    let net = apply_pantry_to_requirements(&requirements, &pantry);
    Ok(optimize_shopping_list(
        &plan.id,
        &net,
        catalog,
        &OptimizeOptions::default(),
    ))
}

/// New ingredients introduced at a given meal (for per-trip reporting).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TripStep {
    pub day: u32,
    pub meal: u32,
    pub recipe_title: String,
    /// Ingredient keys first required by this meal (not in any earlier meal).
    pub new_ingredient_keys: Vec<String>,
    pub new_count: usize,
    pub cumulative_unique: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TripBreakdown {
    pub plan_id: String,
    pub steps: Vec<TripStep>,
    /// Unique ingredients across the whole plan.
    pub total_unique_ingredients: usize,
    /// First-seen ingredient keys summed by plan day (one shopping trip per day).
    pub new_keys_per_day: Vec<usize>,
    /// Human-readable explanation of what the ordering implies for shopping.
    pub summary: String,
}

/// Explain how plan ordering affects when ingredients are first needed (proxy for trips).
///
/// Walks meals in schedule order and records which ingredient keys appear for the
/// first time at each meal. Later meals that reuse those keys contribute 0 new keys
/// — so a min-union construction order shows large `new_count` early and smaller
/// increments later when ingredients are reused.
pub fn trip_breakdown_for_plan(store: &Store, plan: &MealPlan) -> Result<TripBreakdown> {
    let mut steps = Vec::new();
    let mut coverage: HashSet<IngredientKey> = HashSet::new();
    let mut per_day_new: Vec<usize> = vec![0; plan.days.max(1) as usize];

    for m in &plan.meals {
        let recipe = store
            .get_recipe(m.recipe_id.as_str())?
            .ok_or_else(|| anyhow::anyhow!("recipe {} missing", m.recipe_id))?;
        let mut new_keys = Vec::new();
        for line in &recipe.ingredients {
            let key = IngredientKey::from_line(line);
            if coverage.insert(key.clone()) {
                new_keys.push(key.name.clone());
            }
        }
        let n = new_keys.len();
        let day_idx = m.day as usize;
        if day_idx < per_day_new.len() {
            per_day_new[day_idx] += n;
        }
        steps.push(TripStep {
            day: m.day,
            meal: m.meal,
            recipe_title: m.recipe_title.clone(),
            new_count: n,
            new_ingredient_keys: new_keys,
            cumulative_unique: coverage.len(),
        });
    }

    let total = coverage.len();
    let mid = steps.len() / 2;
    let early_new: usize = steps.iter().take(mid).map(|s| s.new_count).sum();
    let late_new: usize = steps.iter().skip(mid).map(|s| s.new_count).sum();
    let summary = if steps.is_empty() {
        "No meals in plan; nothing to analyze.".into()
    } else {
        format!(
            "Walked {} meal(s) in plan order; {} distinct ingredient key(s) in total. \
             New keys introduced per day (trip proxy): {:?}. \
             First half of meals introduced {} new key(s); second half introduced {} \
             (a well-ordered min-union plan typically shows more new keys early, then reuse). \
             Each step lists exactly which keys are first required at that meal — later meals \
             that only reuse earlier ingredients show new_count = 0.",
            steps.len(),
            total,
            per_day_new,
            early_new,
            late_new
        )
    };

    Ok(TripBreakdown {
        plan_id: plan.id.clone(),
        steps,
        total_unique_ingredients: total,
        new_keys_per_day: per_day_new,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Recipe, UnitKind};
    use crate::normalize::normalize_line;
    use crate::pricing::PackageCatalog;
    use tempfile::TempDir;

    fn rec(title: &str, ings: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    #[test]
    fn apply_pantry_subtracts_and_drops_covered() {
        let milk = IngredientKey::new("milk", UnitKind::Volume);
        let eggs = IngredientKey::new("eggs", UnitKind::Count);
        let flour = IngredientKey::new("flour", UnitKind::Mass);
        let req = vec![
            (milk.clone(), 500.0),
            (eggs.clone(), 6.0),
            (flour.clone(), 200.0),
        ];
        let pantry = vec![
            PantryItem {
                key: milk.clone(),
                quantity_canonical: 200.0,
            },
            PantryItem {
                key: eggs.clone(),
                quantity_canonical: 12.0, // fully covers
            },
        ];
        let net = apply_pantry_to_requirements(&req, &pantry);
        assert_eq!(net.len(), 2);
        let milk_need = net.iter().find(|(k, _)| k == &milk).unwrap().1;
        assert!((milk_need - 300.0).abs() < 1e-9);
        let flour_need = net.iter().find(|(k, _)| k == &flour).unwrap().1;
        assert!((flour_need - 200.0).abs() < 1e-9);
        assert!(!net.iter().any(|(k, _)| k == &eggs));
    }

    #[test]
    fn apply_pantry_bridges_mass_stock_to_volume_requirement() {
        // User stocks "500g flour" (Mass); recipe needs flour by volume.
        let flour_vol = IngredientKey::new("flour", UnitKind::Volume);
        let req = vec![(flour_vol.clone(), 100.0)]; // 100 ml
        let pantry = vec![PantryItem {
            key: IngredientKey::new("flour", UnitKind::Mass),
            quantity_canonical: 53.0, // ≈ 100 ml at 0.53 g/ml
        }];
        let net = apply_pantry_to_requirements(&req, &pantry);
        assert!(
            net.is_empty(),
            "mass flour stock should cover volume need via density: {net:?}"
        );
    }

    #[test]
    fn pantry_stock_not_double_credited_across_kinds() {
        // One 250 g flour row must not satisfy BOTH a mass and a volume flour
        // requirement in full.
        let mass = IngredientKey::new("flour", UnitKind::Mass);
        let vol = IngredientKey::new("flour", UnitKind::Volume);
        let req = vec![(mass.clone(), 200.0), (vol.clone(), 236.6)]; // 200 g + 1 cup (~125 g)
        let pantry = vec![PantryItem {
            key: mass.clone(),
            quantity_canonical: 250.0,
        }];
        let net = apply_pantry_to_requirements(&req, &pantry);
        // 200 g consumed exact → 50 g left; the cup needs ~125 g but only 50 g
        // remains → a shortfall must survive on exactly one line.
        let total_remaining: f64 = net.iter().map(|(_, q)| q).sum();
        assert!(total_remaining > 1.0, "double-credit regressed: {net:?}");
    }

    #[test]
    fn consume_from_stock_bridges_and_depletes() {
        let mut stock = vec![PantryItem {
            key: IngredientKey::new("flour", UnitKind::Mass),
            quantity_canonical: 500.0,
        }];
        // cook 1 cup (~236.6 ml → ~125 g) of flour by volume
        let short = consume_from_stock(
            &mut stock,
            &IngredientKey::new("flour", UnitKind::Volume),
            236.6,
        );
        assert!(short < 1e-6, "should be fully covered, short={short}");
        assert!(
            (stock[0].quantity_canonical - 374.6).abs() < 1.0,
            "mass stock should drop by ~125 g: {}",
            stock[0].quantity_canonical
        );
    }

    #[test]
    fn shopping_list_omits_pantry_covered_ingredients() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let a = rec("A", &["1 cup milk", "2 eggs"]);
        store.save_recipe(&a).unwrap();
        // Stock far more milk than one cup needs; leave eggs unstocked.
        let milk_key = IngredientKey::new("milk", UnitKind::Volume);
        store.pantry_set(&milk_key, 10_000.0).unwrap();

        let plan = MealPlan {
            id: "p1".into(),
            days: 1,
            meals_per_day: 1,
            meals: vec![crate::domain::PlannedMeal {
                day: 0,
                meal: 0,
                recipe_id: a.id.clone(),
                recipe_title: a.title.clone(),
            }],
            rationale: "test".into(),
        };
        let list = shopping_list_for_plan(&store, &plan, &PackageCatalog::with_defaults()).unwrap();
        assert!(
            list.items.iter().all(|i| i.ingredient.name != "milk"),
            "milk should be fully covered by pantry: {:?}",
            list.items
        );
        assert!(list.items.iter().any(|i| i.ingredient.name == "eggs"));
    }

    #[test]
    fn restock_leaves_only_package_leftover_then_blocks_repeat() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let a = rec("Shake", &["2 cups milk"]);
        store.save_recipe(&a).unwrap();
        let plan = MealPlan {
            id: "plan-restock-1".into(),
            days: 1,
            meals_per_day: 1,
            meals: vec![crate::domain::PlannedMeal {
                day: 0,
                meal: 0,
                recipe_id: a.id.clone(),
                recipe_title: a.title.clone(),
            }],
            rationale: "test".into(),
        };
        store.save_plan(&plan).unwrap();
        let cat = PackageCatalog::with_defaults();

        let list_before = shopping_list_for_plan(&store, &plan, &cat).unwrap();
        let milk_item = list_before
            .items
            .iter()
            .find(|i| i.ingredient.name == "milk")
            .expect("milk on list");
        let purchased_key_units =
            cat.purchased_to_key_units(&milk_item.ingredient, milk_item.purchased_canonical);
        let required = milk_item.required_canonical;
        // required is in display units for density-converted; milk volume stays ml.
        assert!(purchased_key_units + 1e-6 >= required);

        restock_plan_from_shop(&store, &plan, &cat).unwrap();

        let pantry = store.list_pantry().unwrap();
        let milk_key = IngredientKey::new("milk", UnitKind::Volume);
        let have = pantry_quantity_for(&milk_key, &pantry);
        let expected_leftover = purchased_key_units - {
            // gross requirement in key units (2 cups)
            let line = normalize_line("2 cups milk");
            line.canonical_quantity().unwrap().0
        };
        assert!(
            (have - expected_leftover).abs() < 1.0,
            "after restock expect leftover ~{expected_leftover}, got {have}"
        );

        // Re-shop same plan must buy the cooked amount again (only leftover remains).
        let list_after = shopping_list_for_plan(&store, &plan, &cat).unwrap();
        let milk_after = list_after
            .items
            .iter()
            .find(|i| i.ingredient.name == "milk");
        assert!(
            milk_after.is_some(),
            "must need to buy milk again after cooking; list={:?}",
            list_after.items
        );

        // Second restock is rejected (idempotent guard).
        assert!(restock_plan_from_shop(&store, &plan, &cat).is_err());
    }

    #[test]
    fn compute_restock_delta_adds_purchases_deducts_gross() {
        // Count ingredients are not density-converted; quantities pass through.
        let eggs = IngredientKey::new("eggs", UnitKind::Count);
        let gross = vec![(eggs.clone(), 6.0)];
        let list = ShoppingList {
            plan_id: "p".into(),
            items: vec![crate::domain::ShoppingItem {
                ingredient: eggs.clone(),
                required_canonical: 6.0,
                required_unit_label: "ea".into(),
                packages: vec![],
                purchased_canonical: 12.0,
                leftover_canonical: 6.0,
                total_cost_cents: None,
                leftover_flagged: true,
            }],
            total_cost_cents: None,
        };
        let delta = compute_restock_delta(&gross, &list, &PackageCatalog::with_defaults());
        assert_eq!(delta.additions.len(), 1);
        assert!((delta.additions[0].1 - 12.0).abs() < 1e-6);
        assert_eq!(delta.deductions.len(), 1);
        assert!((delta.deductions[0].1 - 6.0).abs() < 1e-6);
    }

    #[test]
    fn trip_steps_mark_reuse_as_zero_new() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let a = rec("A", &["1 cup milk", "2 eggs"]);
        let b = rec("B", &["1 cup milk", "1 cup flour"]);
        store.save_recipe(&a).unwrap();
        store.save_recipe(&b).unwrap();
        let plan = MealPlan {
            id: "p1".into(),
            days: 2,
            meals_per_day: 1,
            meals: vec![
                crate::domain::PlannedMeal {
                    day: 0,
                    meal: 0,
                    recipe_id: a.id.clone(),
                    recipe_title: a.title.clone(),
                },
                crate::domain::PlannedMeal {
                    day: 1,
                    meal: 0,
                    recipe_id: b.id.clone(),
                    recipe_title: b.title.clone(),
                },
            ],
            rationale: "test".into(),
        };
        let t = trip_breakdown_for_plan(&store, &plan).unwrap();
        assert_eq!(t.steps[0].new_count, 2); // milk, eggs
        assert_eq!(t.steps[1].new_count, 1); // flour only; milk reused
        assert_eq!(t.total_unique_ingredients, 3);
        assert!(!t.summary.contains("positive advantage"));
        assert!(!t.summary.contains("reversed"));
        // Ingredient keys read as plain names, not "name (Kind)".
        assert!(t.steps[0]
            .new_ingredient_keys
            .iter()
            .all(|k| !k.contains('(')));
        assert!(t.steps[0].new_ingredient_keys.contains(&"milk".to_string()));
    }
}
