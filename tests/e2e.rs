//! End-to-end: import → plan → shop against a temp database.

use smarter_recipes::ingest::ingest_from;
use smarter_recipes::planning::{plan_meals, PlanOptions};
use smarter_recipes::pricing::PackageCatalog;
use smarter_recipes::shopping::shopping_list_for_plan;
use smarter_recipes::storage::Store;
use tempfile::TempDir;

#[test]
fn import_plan_shop() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db).unwrap();

    for path in [
        "recipes/pancakes.json",
        "recipes/french_toast.json",
        "recipes/tomato_pasta.json",
        "recipes/garlic_bread.toml",
        "recipes/chicken_rice.json",
        "recipes/omelette.txt",
    ] {
        let r = ingest_from("file", path).unwrap();
        store.save_recipe(&r).unwrap();
    }

    let pool = store.list_recipes(None).unwrap();
    assert_eq!(pool.len(), 6);

    let plan = plan_meals(
        &pool,
        &PlanOptions {
            days: 4,
            meals_per_day: 1,
            ..Default::default()
        },
    );
    assert_eq!(plan.meals.len(), 4);
    let unique_ids: std::collections::HashSet<_> =
        plan.meals.iter().map(|m| m.recipe_id.as_str()).collect();
    assert_eq!(
        unique_ids.len(),
        plan.meals.len(),
        "plan must not repeat recipes"
    );
    assert!(
        plan.rationale.to_lowercase().contains("min-union")
            || plan.rationale.to_lowercase().contains("no recipe repeats"),
        "rationale should describe min-union / no-repeat planner: {}",
        plan.rationale
    );
    store.save_plan(&plan).unwrap();

    let list = shopping_list_for_plan(&store, &plan, &PackageCatalog::with_defaults()).unwrap();
    assert!(!list.items.is_empty());
    for item in &list.items {
        assert!(item.purchased_canonical + 1e-6 >= item.required_canonical);
    }
    assert!(list.total_cost_cents.is_some());
}
