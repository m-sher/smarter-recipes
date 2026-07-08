//! Tauri backend: thin commands over `smarter_recipes::storage`.

use serde::Serialize;
use smarter_recipes::domain::{ShoppingList, UnitKind};
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
pub struct PantryItemView {
    pub name: String,
    pub kind: String,
    pub quantity_canonical: f64,
    pub unit_label: String,
}

fn kind_str(k: UnitKind) -> &'static str {
    match k {
        UnitKind::Mass => "mass",
        UnitKind::Volume => "volume",
        UnitKind::Count => "count",
        UnitKind::Other => "other",
    }
}

fn open_default_store() -> Result<Store, String> {
    let path = Store::default_path();
    Store::open(path).map_err(|e| format!("open database: {e:#}"))
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
        .invoke_handler(tauri::generate_handler![get_status, list_recipes, list_pantry])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
