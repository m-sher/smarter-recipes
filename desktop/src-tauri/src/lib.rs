//! Tauri backend: thin commands over `smarter_recipes` (store, plan, shop, ingest).

use serde::{Deserialize, Serialize};
use smarter_recipes::domain::{IngredientKey, Macros, Recipe, RecipeId, ShoppingList, UnitKind};
use smarter_recipes::ingest::{ingest_from, ingest_many};
use smarter_recipes::normalize::normalize_line;
use smarter_recipes::nutrition::{recipe_nutrition, source_recipe_macros};
use smarter_recipes::planning::{
    load_nutrition_bounds, plan_meals, CliPerDayNutrition, PlanOptions, MIN_INGREDIENT_COVERAGE,
};
use smarter_recipes::pricing::PackageCatalog;
use smarter_recipes::shopping::{restock_plan_from_shop, shopping_list_for_plan};
use smarter_recipes::storage::Store;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::State;

pub struct AppState {
    pub store: Mutex<Store>,
}

#[derive(Debug, Serialize)]
pub struct DbStatus {
    pub path: String,
    pub recipe_count: usize,
    pub plan_count: usize,
    pub pantry_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RecipeSummary {
    pub id: String,
    pub title: String,
    pub category: Option<String>,
    pub ingredient_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RecipeDetail {
    pub id: String,
    pub title: String,
    pub category: Option<String>,
    pub servings: Option<f64>,
    pub ingredients: Vec<String>,
    pub steps: Vec<String>,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct PantryItemView {
    pub name: String,
    pub kind: String,
    pub quantity_canonical: f64,
    pub unit_label: String,
}

#[derive(Debug, Serialize)]
pub struct PlannedMealView {
    pub day: u32,
    pub meal: u32,
    pub recipe_id: String,
    pub recipe_title: String,
    pub uses_pantry: bool,
}

#[derive(Debug, Serialize)]
pub struct PlanView {
    pub id: String,
    pub days: u32,
    pub meals_per_day: u32,
    pub meals: Vec<PlannedMealView>,
    pub rationale: String,
}

#[derive(Debug, Serialize)]
pub struct PlanSummary {
    pub id: String,
    pub days: u32,
    pub meals_per_day: u32,
    pub meal_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct CreatePlanArgs {
    pub days: u32,
    pub meals_per_day: u32,
    pub time_of_day: bool,
    pub save: bool,
    pub nutrition_config: Option<String>,
    pub min_protein_g: Option<f64>,
    pub max_kcal: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ShopItemView {
    pub name: String,
    pub need: f64,
    pub unit: String,
    pub leftover: f64,
}

#[derive(Debug, Serialize)]
pub struct RestockResult {
    pub additions: usize,
    pub deductions: usize,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub saved: usize,
    pub titles: Vec<String>,
    pub message: String,
}

fn kind_str(k: UnitKind) -> &'static str {
    match k {
        UnitKind::Mass => "mass",
        UnitKind::Volume => "volume",
        UnitKind::Count => "count",
        UnitKind::Other => "other",
    }
}

fn parse_kind(s: &str) -> Result<UnitKind, String> {
    match s.to_lowercase().as_str() {
        "mass" | "g" | "weight" => Ok(UnitKind::Mass),
        "volume" | "vol" | "ml" => Ok(UnitKind::Volume),
        "count" | "ea" | "each" => Ok(UnitKind::Count),
        "other" => Ok(UnitKind::Other),
        other => Err(format!("unknown kind '{other}'")),
    }
}

fn parse_pantry_line(line: &str) -> Result<(IngredientKey, f64), String> {
    let parsed = normalize_line(line);
    let key = IngredientKey::from_line(&parsed);
    let Some((qty, _)) = parsed.canonical_quantity() else {
        return Err("need a quantity and unit (e.g. \"2 cups milk\")".into());
    };
    if qty <= 0.0 {
        return Err("quantity must be positive".into());
    }
    Ok((key, qty))
}

fn open_default_store() -> Result<Store, String> {
    Store::open(Store::default_path()).map_err(|e| format!("open database: {e:#}"))
}

fn source_label(r: &Recipe) -> String {
    match &r.source {
        smarter_recipes::domain::RecipeSource::Url { url } => format!("url:{url}"),
        smarter_recipes::domain::RecipeSource::Epub { path, .. } => format!("epub:{path}"),
        smarter_recipes::domain::RecipeSource::File { path } => format!("file:{path}"),
        smarter_recipes::domain::RecipeSource::Image { path } => format!("image:{path}"),
        smarter_recipes::domain::RecipeSource::Manual => "manual".into(),
        smarter_recipes::domain::RecipeSource::Unknown => "unknown".into(),
    }
}

fn plan_to_view(plan: &smarter_recipes::domain::MealPlan) -> PlanView {
    PlanView {
        id: plan.id.clone(),
        days: plan.days,
        meals_per_day: plan.meals_per_day,
        meals: plan
            .meals
            .iter()
            .map(|m| PlannedMealView {
                day: m.day,
                meal: m.meal,
                recipe_id: m.recipe_id.as_str().to_string(),
                recipe_title: m.recipe_title.clone(),
                uses_pantry: m.uses_pantry,
            })
            .collect(),
        rationale: plan.rationale.clone(),
    }
}

fn resolve_recipe_prefix(store: &Store, id: &str) -> Result<Recipe, String> {
    let all = store.list_recipes(None).map_err(|e| e.to_string())?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|r| r.id.as_str().starts_with(id))
        .collect();
    match matches.len() {
        0 => Err(format!("no recipe matching '{id}'")),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(format!("ambiguous id '{id}' ({n} matches)")),
    }
}

fn resolve_plan_prefix(
    store: &Store,
    id: &str,
) -> Result<smarter_recipes::domain::MealPlan, String> {
    let plans = store.list_plans().map_err(|e| e.to_string())?;
    let matches: Vec<_> = plans
        .into_iter()
        .filter(|p| p.id.starts_with(id))
        .collect();
    match matches.len() {
        0 => Err(format!("no plan matching '{id}'")),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(format!("ambiguous plan id '{id}' ({n} matches)")),
    }
}

fn nutrition_extra(store: &Store) -> HashMap<String, Macros> {
    store
        .nutrition_cache_all()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.map(|m| (k, m)))
        .collect()
}

fn recipe_macros_for_pool(
    recipes: &[Recipe],
    extra: &HashMap<String, Macros>,
) -> (HashMap<RecipeId, Macros>, HashSet<RecipeId>) {
    let mut macros = HashMap::new();
    let mut low_coverage = HashSet::new();
    for r in recipes {
        let n = recipe_nutrition(r, extra);
        let estimable = n.covered.len() + n.uncovered.len();
        let coverage = if estimable > 0 {
            n.covered.len() as f64 / estimable as f64
        } else {
            0.0
        };
        if let Some(src) = source_recipe_macros(r) {
            macros.insert(r.id.clone(), src);
        } else {
            if estimable > 0 && coverage < MIN_INGREDIENT_COVERAGE {
                low_coverage.insert(r.id.clone());
            }
            macros.insert(r.id.clone(), n.macros);
        }
    }
    (macros, low_coverage)
}

fn pantry_views(store: &Store) -> Result<Vec<PantryItemView>, String> {
    let items = store.list_pantry().map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|p| PantryItemView {
            name: p.key.name.clone(),
            kind: kind_str(p.key.kind).to_string(),
            quantity_canonical: p.quantity_canonical,
            unit_label: ShoppingList::kind_label(p.key.kind).to_string(),
        })
        .collect())
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Result<DbStatus, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let recipes = store.list_recipes(None).map_err(|e| e.to_string())?;
    let plans = store.list_plans().map_err(|e| e.to_string())?;
    let pantry = store.list_pantry().map_err(|e| e.to_string())?;
    Ok(DbStatus {
        path: store.path().display().to_string(),
        recipe_count: recipes.len(),
        plan_count: plans.len(),
        pantry_count: pantry.len(),
    })
}

#[tauri::command]
fn list_recipes(
    state: State<'_, AppState>,
    filter: Option<String>,
) -> Result<Vec<RecipeSummary>, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let recipes = store
        .list_recipes(filter.as_deref())
        .map_err(|e| e.to_string())?;
    Ok(recipes
        .into_iter()
        .map(|r| RecipeSummary {
            id: r.id.as_str().to_string(),
            title: r.title,
            category: r.meta.category,
            ingredient_count: r.ingredients.len(),
        })
        .collect())
}

#[tauri::command]
fn get_recipe(state: State<'_, AppState>, id: String) -> Result<RecipeDetail, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let r = resolve_recipe_prefix(&store, &id)?;
    Ok(RecipeDetail {
        id: r.id.as_str().to_string(),
        title: r.title,
        category: r.meta.category,
        servings: r.servings,
        ingredients: r.ingredients.iter().map(|l| l.original.clone()).collect(),
        steps: r.steps,
        source: source_label(&r),
    })
}

#[tauri::command]
fn delete_recipe(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let r = resolve_recipe_prefix(&store, &id)?;
    store
        .delete_recipe(r.id.as_str())
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn list_pantry(state: State<'_, AppState>) -> Result<Vec<PantryItemView>, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    pantry_views(&store)
}

#[tauri::command]
fn pantry_add(state: State<'_, AppState>, line: String) -> Result<Vec<PantryItemView>, String> {
    let (key, qty) = parse_pantry_line(&line)?;
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    store.pantry_add(&key, qty).map_err(|e| e.to_string())?;
    pantry_views(&store)
}

#[tauri::command]
fn pantry_remove(
    state: State<'_, AppState>,
    name: String,
    kind: Option<String>,
) -> Result<Vec<PantryItemView>, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let items = store.list_pantry().map_err(|e| e.to_string())?;
    let name_key = name.trim().to_lowercase();
    let candidates: Vec<_> = items
        .iter()
        .filter(|p| p.key.name.to_lowercase() == name_key)
        .collect();
    let key = if let Some(k) = kind {
        let kind = parse_kind(&k)?;
        candidates
            .iter()
            .find(|p| p.key.kind == kind)
            .map(|p| p.key.clone())
            .ok_or_else(|| format!("no pantry item '{name}' with kind {k}"))?
    } else {
        match candidates.len() {
            0 => return Err(format!("no pantry item matching '{name}'")),
            1 => candidates[0].key.clone(),
            _ => {
                return Err(format!(
                    "ambiguous pantry name '{name}'; pass kind (mass/volume/count)"
                ))
            }
        }
    };
    store.pantry_remove(&key).map_err(|e| e.to_string())?;
    pantry_views(&store)
}

#[tauri::command]
fn list_plans(state: State<'_, AppState>) -> Result<Vec<PlanSummary>, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let plans = store.list_plans().map_err(|e| e.to_string())?;
    Ok(plans
        .into_iter()
        .map(|p| PlanSummary {
            id: p.id,
            days: p.days,
            meals_per_day: p.meals_per_day,
            meal_count: p.meals.len(),
        })
        .collect())
}

#[tauri::command]
fn get_plan(state: State<'_, AppState>, id: String) -> Result<PlanView, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    Ok(plan_to_view(&resolve_plan_prefix(&store, &id)?))
}

#[tauri::command]
fn create_plan(state: State<'_, AppState>, args: CreatePlanArgs) -> Result<PlanView, String> {
    if args.days == 0 || args.meals_per_day == 0 {
        return Err("days and meals_per_day must be >= 1".into());
    }
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let recipes = store.list_recipes(None).map_err(|e| e.to_string())?;
    if recipes.is_empty() {
        return Err("recipe pool is empty; import recipes first".into());
    }
    let pantry = store.list_pantry().map_err(|e| e.to_string())?;

    let cli_nutrition = CliPerDayNutrition {
        min_kcal: None,
        max_kcal: args.max_kcal,
        min_protein_g: args.min_protein_g,
        max_protein_g: None,
        min_fat_g: None,
        max_fat_g: None,
        min_carbs_g: None,
        max_carbs_g: None,
    };
    let nutrition_path = args
        .nutrition_config
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let nutrition = load_nutrition_bounds(nutrition_path.as_deref(), &cli_nutrition)
        .map_err(|e| format!("nutrition config: {e:#}"))?;

    let extra = nutrition_extra(&store);
    let (recipe_macros, recipe_low_coverage) = recipe_macros_for_pool(&recipes, &extra);

    let opts = PlanOptions {
        days: args.days,
        meals_per_day: args.meals_per_day,
        pantry,
        nutrition,
        recipe_macros,
        recipe_low_coverage,
        time_of_day: args.time_of_day,
    };
    let plan = plan_meals(&recipes, &opts);
    if args.save {
        store.save_plan(&plan).map_err(|e| e.to_string())?;
    }
    Ok(plan_to_view(&plan))
}

#[tauri::command]
fn shop_plan(state: State<'_, AppState>, id: String) -> Result<Vec<ShopItemView>, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let plan = resolve_plan_prefix(&store, &id)?;
    let cat = PackageCatalog::with_defaults();
    let list = shopping_list_for_plan(&store, &plan, &cat).map_err(|e| e.to_string())?;
    Ok(list
        .items
        .into_iter()
        .map(|i| ShopItemView {
            name: i.ingredient.name,
            need: i.required_canonical,
            unit: i.required_unit_label,
            leftover: i.leftover_canonical,
        })
        .collect())
}

#[tauri::command]
fn restock_plan(state: State<'_, AppState>, id: String) -> Result<RestockResult, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let plan = resolve_plan_prefix(&store, &id)?;
    let cat = PackageCatalog::with_defaults();
    let delta = restock_plan_from_shop(&store, &plan, &cat).map_err(|e| e.to_string())?;
    Ok(RestockResult {
        additions: delta.additions.len(),
        deductions: delta.deductions.len(),
        message: format!(
            "Restocked: {} purchase line(s), {} cooked deduction(s). Leftovers remain in pantry.",
            delta.additions.len(),
            delta.deductions.len()
        ),
    })
}

#[tauri::command]
fn import_source(
    state: State<'_, AppState>,
    source: String,
    input: String,
) -> Result<ImportResult, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("path or URL required".into());
    }
    let source = source.trim().to_lowercase();
    let batch = if source == "epub" || source == "ebook" || (source == "auto" && input.ends_with(".epub")) {
        ingest_many(&source, input).map_err(|e| e.to_string())?
    } else {
        let r = ingest_from(if source.is_empty() { "auto" } else { &source }, input)
            .map_err(|e| e.to_string())?;
        smarter_recipes::ingest::IngestBatch {
            recipes: vec![r],
            skipped_ambiguous: Vec::new(),
        }
    };
    if batch.recipes.is_empty() {
        return Err("no recipes ingested".into());
    }
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    let mut titles = Vec::new();
    let mut saved = 0usize;
    for r in &batch.recipes {
        if store
            .is_duplicate(r.meta.source_url.as_deref())
            .map_err(|e| e.to_string())?
        {
            continue;
        }
        store.save_recipe(r).map_err(|e| e.to_string())?;
        titles.push(r.title.clone());
        saved += 1;
    }
    Ok(ImportResult {
        saved,
        titles,
        message: format!(
            "Saved {saved} recipe(s){}{}",
            if batch.skipped_ambiguous.is_empty() {
                String::new()
            } else {
                format!(", skipped {} ambiguous", batch.skipped_ambiguous.len())
            },
            if saved == 0 {
                " (all duplicates?)"
            } else {
                ""
            }
        ),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = open_default_store().unwrap_or_else(|e| {
        eprintln!("warning: {e}; creating empty DB at default path");
        let path = Store::default_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Store::open(&path).expect("failed to open or create database")
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            store: Mutex::new(store),
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_recipes,
            get_recipe,
            delete_recipe,
            list_pantry,
            pantry_add,
            pantry_remove,
            list_plans,
            get_plan,
            create_plan,
            shop_plan,
            restock_plan,
            import_source,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
