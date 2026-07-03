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

#[test]
fn plan_respects_per_day_protein_config() {
    use smarter_recipes::domain::{Macros, RecipeId};
    use smarter_recipes::planning::{
        plan_bound_violations, plan_meals, NutritionBounds, PlanOptions,
    };
    use std::collections::HashMap;

    let dessert1 = {
        let mut r = smarter_recipes::domain::Recipe::new("Cake");
        r.id = RecipeId::from("d1");
        r.ingredients = vec![smarter_recipes::normalize::normalize_line("100 g sugar")];
        r
    };
    let dessert2 = {
        let mut r = smarter_recipes::domain::Recipe::new("Cookies");
        r.id = RecipeId::from("d2");
        r.ingredients = vec![
            smarter_recipes::normalize::normalize_line("100 g sugar"),
            smarter_recipes::normalize::normalize_line("50 g flour"),
        ];
        r
    };
    let protein = {
        let mut r = smarter_recipes::domain::Recipe::new("Chicken Rice");
        r.id = RecipeId::from("p1");
        r.ingredients = vec![
            smarter_recipes::normalize::normalize_line("200 g chicken"),
            smarter_recipes::normalize::normalize_line("100 g rice"),
        ];
        r
    };
    let mut macros = HashMap::new();
    macros.insert(
        RecipeId::from("d1"),
        Macros {
            kcal: 800.0,
            protein_g: 5.0,
            ..Default::default()
        },
    );
    macros.insert(
        RecipeId::from("d2"),
        Macros {
            kcal: 700.0,
            protein_g: 4.0,
            ..Default::default()
        },
    );
    macros.insert(
        RecipeId::from("p1"),
        Macros {
            kcal: 500.0,
            protein_g: 60.0,
            ..Default::default()
        },
    );
    let nutrition = NutritionBounds::from_toml_str(
        r#"
        [per_day]
        protein_g = { min = 50.0 }
        "#,
    )
    .unwrap();
    let pool = vec![dessert1, dessert2, protein];
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
    assert!(plan.meals.iter().any(|m| m.recipe_title == "Chicken Rice"));
    assert!(plan_bound_violations(&pool, &plan, &nutrition, &macros).is_empty());
}
