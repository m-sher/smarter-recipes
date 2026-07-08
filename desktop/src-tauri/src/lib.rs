//! Tauri backend: thin commands over `smarter_recipes` storage + planning.

use serde::{Deserialize, Serialize};
use smarter_recipes::domain::{IngredientKey, ShoppingList, UnitKind};
use smarter_recipes::normalize::normalize_line;
use smarter_recipes::planning::{plan_meals, PlanOptions};
use smarter_recipes::pricing::PackageCatalog;
use smarter_recipes::shopping::shopping_list_for_plan;
use smarter_recipes::storage::Store;
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
}

#[derive(Debug, Serialize)]
pub struct ShopItemView {
    pub name: String,
    pub need: f64,
    pub unit: String,
    pub leftover: f64,
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
    let path = Store::default_path();
    Store::open(path).map_err(|e| format!("open database: {e:#}"))
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
    // Prefix match like CLI.
    let all = store.list_recipes(None).map_err(|e| e.to_string())?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|r| r.id.as_str().starts_with(id.as_str()))
        .collect();
    let r = match matches.len() {
        0 => return Err(format!("no recipe matching '{id}'")),
        1 => matches.into_iter().next().unwrap(),
        n => return Err(format!("ambiguous id '{id}' ({n} matches)")),
    };
    let source = match &r.source {
        smarter_recipes::domain::RecipeSource::Url { url } => format!("url:{url}"),
        smarter_recipes::domain::RecipeSource::Epub { path, .. } => format!("epub:{path}"),
        smarter_recipes::domain::RecipeSource::File { path } => format!("file:{path}"),
        smarter_recipes::domain::RecipeSource::Image { path } => format!("image:{path}"),
        smarter_recipes::domain::RecipeSource::Manual => "manual".into(),
        smarter_recipes::domain::RecipeSource::Unknown => "unknown".into(),
    };
    Ok(RecipeDetail {
        id: r.id.as_str().to_string(),
        title: r.title,
        category: r.meta.category,
        servings: r.servings,
        ingredients: r.ingredients.iter().map(|l| l.original.clone()).collect(),
        steps: r.steps,
        source,
    })
}

#[tauri::command]
fn list_pantry(state: State<'_, AppState>) -> Result<Vec<PantryItemView>, String> {
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
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
fn pantry_add(state: State<'_, AppState>, line: String) -> Result<Vec<PantryItemView>, String> {
    let (key, qty) = parse_pantry_line(&line)?;
    let store = state.store.lock().map_err(|_| "database lock poisoned")?;
    store.pantry_add(&key, qty).map_err(|e| e.to_string())?;
    drop(store);
    list_pantry(state)
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
    drop(store);
    list_pantry(state)
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
    let plans = store.list_plans().map_err(|e| e.to_string())?;
    let matches: Vec<_> = plans
        .into_iter()
        .filter(|p| p.id.starts_with(id.as_str()))
        .collect();
    let plan = match matches.len() {
        0 => return Err(format!("no plan matching '{id}'")),
        1 => matches.into_iter().next().unwrap(),
        n => return Err(format!("ambiguous plan id '{id}' ({n} matches)")),
    };
    Ok(plan_to_view(&plan))
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
    let opts = PlanOptions {
        days: args.days,
        meals_per_day: args.meals_per_day,
        pantry,
        time_of_day: args.time_of_day,
        ..Default::default()
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
    let plans = store.list_plans().map_err(|e| e.to_string())?;
    let matches: Vec<_> = plans
        .into_iter()
        .filter(|p| p.id.starts_with(id.as_str()))
        .collect();
    let plan = match matches.len() {
        0 => return Err(format!("no plan matching '{id}'")),
        1 => matches.into_iter().next().unwrap(),
        n => return Err(format!("ambiguous plan id '{id}' ({n} matches)")),
    };
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
            list_pantry,
            pantry_add,
            pantry_remove,
            list_plans,
            get_plan,
            create_plan,
            shop_plan,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
