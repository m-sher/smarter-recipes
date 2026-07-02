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
        },
    );
    assert_eq!(plan.meals.len(), 4);
    store.save_plan(&plan).unwrap();

    let list = shopping_list_for_plan(&store, &plan, &PackageCatalog::with_defaults()).unwrap();
    assert!(!list.items.is_empty());
    for item in &list.items {
        assert!(item.purchased_canonical + 1e-6 >= item.required_canonical);
    }
    assert!(list.total_cost_cents.is_some());
}
