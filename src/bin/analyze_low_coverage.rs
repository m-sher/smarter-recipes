use smarter_recipes::nutrition::{recipe_nutrition, source_recipe_macros};
use smarter_recipes::planning::MIN_INGREDIENT_COVERAGE;
use smarter_recipes::storage::Store;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let db = env::var("SMARTER_RECIPES_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env::var("HOME").unwrap()).join(".local/share/smarter-recipes/recipes.db")
        });
    let store = Store::open(&db)?;
    let recipes = store.list_recipes(None)?;
    let extra: HashMap<_, _> = store
        .nutrition_cache_all()?
        .into_iter()
        .filter_map(|(k, v)| v.map(|m| (k, m)))
        .collect();
    let mut low = 0usize;
    let mut high = 0usize;
    let mut source = 0usize;
    let mut zero = 0usize;
    for r in &recipes {
        if source_recipe_macros(r).is_some() {
            source += 1;
            continue;
        }
        let n = recipe_nutrition(r, &extra);
        let est = n.covered.len() + n.uncovered.len();
        if est == 0 {
            zero += 1;
            continue;
        }
        let cov = n.covered.len() as f64 / est as f64;
        if cov >= MIN_INGREDIENT_COVERAGE {
            high += 1;
        } else {
            low += 1;
        }
    }
    println!("total={}", recipes.len());
    println!("source_nutrition={source}");
    println!("high_coverage={high}");
    println!("low_coverage={low}");
    println!("zero_estimable={zero}");
    println!("pool_if_bounds={}", source + high);
    Ok(())
}
