//! SQLite persistence for recipes, ingredients, and meal plans.
//!
//! Ingredients are deduplicated by `(normalized_name, unit_kind)` so quantities
//! can be aggregated across recipes and plans.

use crate::domain::{
    normalize_title_key, IngredientKey, IngredientLine, MealPlan, PlannedMeal, Recipe, RecipeId,
    RecipeMeta, RecipeSource, Unit, UnitKind,
};
use crate::ingest::{normalize_url, recipe_source_url};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ingredients (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    UNIQUE(name, kind)
);

CREATE TABLE IF NOT EXISTS recipes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    servings REAL,
    steps_json TEXT NOT NULL,
    meta_json TEXT NOT NULL,
    source_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS recipe_ingredients (
    recipe_id TEXT NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    ingredient_id INTEGER NOT NULL REFERENCES ingredients(id),
    original TEXT NOT NULL,
    quantity REAL,
    unit_name TEXT,
    unit_to_base REAL,
    note TEXT,
    parse_uncertain INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (recipe_id, position)
);

CREATE TABLE IF NOT EXISTS meal_plans (
    id TEXT PRIMARY KEY,
    days INTEGER NOT NULL,
    meals_per_day INTEGER NOT NULL,
    rationale TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS plan_meals (
    plan_id TEXT NOT NULL REFERENCES meal_plans(id) ON DELETE CASCADE,
    day INTEGER NOT NULL,
    meal INTEGER NOT NULL,
    recipe_id TEXT NOT NULL REFERENCES recipes(id),
    recipe_title TEXT NOT NULL,
    PRIMARY KEY (plan_id, day, meal)
);

CREATE TABLE IF NOT EXISTS scrape_failures (
    url TEXT PRIMARY KEY,
    reason TEXT NOT NULL,
    failed_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_recipes_title ON recipes(title);
CREATE INDEX IF NOT EXISTS idx_ri_ingredient ON recipe_ingredients(ingredient_id);
"#;

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (or create) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("opening database at {}", path.display()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, path })
    }

    /// Default location: `~/.local/share/smarter-recipes/recipes.db` or `./data/recipes.db`.
    pub fn default_path() -> PathBuf {
        if let Some(dir) = dirs::data_local_dir() {
            dir.join("smarter-recipes").join("recipes.db")
        } else {
            PathBuf::from("data/recipes.db")
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn kind_str(k: UnitKind) -> &'static str {
        match k {
            UnitKind::Mass => "mass",
            UnitKind::Volume => "volume",
            UnitKind::Count => "count",
            UnitKind::Other => "other",
        }
    }

    fn parse_kind(s: &str) -> UnitKind {
        match s {
            "mass" => UnitKind::Mass,
            "volume" => UnitKind::Volume,
            "count" => UnitKind::Count,
            _ => UnitKind::Other,
        }
    }

    /// Insert or fetch ingredient row; returns ingredient id.
    pub fn upsert_ingredient(&self, key: &IngredientKey) -> Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO ingredients (name, kind) VALUES (?1, ?2)",
            params![key.name, Self::kind_str(key.kind)],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM ingredients WHERE name = ?1 AND kind = ?2",
            params![key.name, Self::kind_str(key.kind)],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Persist a recipe (insert or replace by id). Dedups ingredient identities.
    pub fn save_recipe(&self, recipe: &Recipe) -> Result<()> {
        let steps_json = serde_json::to_string(&recipe.steps)?;
        let meta_json = serde_json::to_string(&recipe.meta)?;
        let source_json = serde_json::to_string(&recipe.source)?;

        self.conn.execute(
            "INSERT INTO recipes (id, title, servings, steps_json, meta_json, source_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title,
               servings=excluded.servings,
               steps_json=excluded.steps_json,
               meta_json=excluded.meta_json,
               source_json=excluded.source_json",
            params![
                recipe.id.as_str(),
                recipe.title,
                recipe.servings,
                steps_json,
                meta_json,
                source_json
            ],
        )?;

        self.conn.execute(
            "DELETE FROM recipe_ingredients WHERE recipe_id = ?1",
            params![recipe.id.as_str()],
        )?;

        for (pos, line) in recipe.ingredients.iter().enumerate() {
            let key = IngredientKey::from_line(line);
            let ing_id = self.upsert_ingredient(&key)?;
            let (unit_name, unit_to_base) = match &line.unit {
                Some(u) => (Some(u.name.clone()), Some(u.to_base)),
                None => (None, None),
            };
            self.conn.execute(
                "INSERT INTO recipe_ingredients
                 (recipe_id, position, ingredient_id, original, quantity, unit_name, unit_to_base, note, parse_uncertain)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    recipe.id.as_str(),
                    pos as i64,
                    ing_id,
                    line.original,
                    line.quantity,
                    unit_name,
                    unit_to_base,
                    line.note,
                    line.parse_uncertain as i64
                ],
            )?;
        }
        Ok(())
    }

    fn load_recipe_row(
        &self,
        id: &str,
        title: String,
        servings: Option<f64>,
        steps_json: String,
        meta_json: String,
        source_json: String,
    ) -> Result<Recipe> {
        let steps: Vec<String> = serde_json::from_str(&steps_json)?;
        let meta: RecipeMeta = serde_json::from_str(&meta_json)?;
        let source: RecipeSource = serde_json::from_str(&source_json)?;

        let mut stmt = self.conn.prepare(
            "SELECT ri.original, ri.quantity, ri.unit_name, ri.unit_to_base, ri.note, ri.parse_uncertain,
                    i.name, i.kind
             FROM recipe_ingredients ri
             JOIN ingredients i ON i.id = ri.ingredient_id
             WHERE ri.recipe_id = ?1
             ORDER BY ri.position",
        )?;

        let ingredients = stmt
            .query_map(params![id], |row| {
                let original: String = row.get(0)?;
                let quantity: Option<f64> = row.get(1)?;
                let unit_name: Option<String> = row.get(2)?;
                let unit_to_base: Option<f64> = row.get(3)?;
                let note: Option<String> = row.get(4)?;
                let uncertain: i64 = row.get(5)?;
                let name: String = row.get(6)?;
                let kind_s: String = row.get(7)?;
                let kind = Self::parse_kind(&kind_s);
                let unit = match (unit_name, unit_to_base) {
                    (Some(n), Some(tb)) => Some(Unit::new(n, kind, tb)),
                    _ => None,
                };
                Ok(IngredientLine {
                    original,
                    name,
                    quantity,
                    unit,
                    note,
                    parse_uncertain: uncertain != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(Recipe {
            id: RecipeId(id.to_string()),
            title,
            servings,
            ingredients,
            steps,
            meta,
            source,
        })
    }

    pub fn get_recipe(&self, id: &str) -> Result<Option<Recipe>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, title, servings, steps_json, meta_json, source_json FROM recipes WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<f64>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;

        match row {
            Some((id, title, servings, steps, meta, source)) => Ok(Some(
                self.load_recipe_row(&id, title, servings, steps, meta, source)?,
            )),
            None => Ok(None),
        }
    }

    /// List recipes; optional substring filter on title (case-insensitive).
    pub fn list_recipes(&self, filter: Option<&str>) -> Result<Vec<Recipe>> {
        let mut sql = String::from(
            "SELECT id, title, servings, steps_json, meta_json, source_json FROM recipes",
        );
        if filter.is_some() {
            sql.push_str(" WHERE lower(title) LIKE ?1");
        }
        sql.push_str(" ORDER BY title COLLATE NOCASE");

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = if let Some(f) = filter {
            let pat = format!("%{}%", f.to_lowercase());
            stmt.query_map(params![pat], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };

        let mut out = Vec::with_capacity(rows.len());
        for (id, title, servings, steps, meta, source) in rows {
            out.push(self.load_recipe_row(&id, title, servings, steps, meta, source)?);
        }
        Ok(out)
    }

    pub fn delete_recipe(&self, id: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM recipes WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    pub fn save_plan(&self, plan: &MealPlan) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meal_plans (id, days, meals_per_day, rationale)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
               days=excluded.days,
               meals_per_day=excluded.meals_per_day,
               rationale=excluded.rationale",
            params![
                plan.id,
                plan.days as i64,
                plan.meals_per_day as i64,
                plan.rationale
            ],
        )?;
        self.conn.execute(
            "DELETE FROM plan_meals WHERE plan_id = ?1",
            params![plan.id],
        )?;
        for m in &plan.meals {
            self.conn.execute(
                "INSERT INTO plan_meals (plan_id, day, meal, recipe_id, recipe_title)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    plan.id,
                    m.day as i64,
                    m.meal as i64,
                    m.recipe_id.as_str(),
                    m.recipe_title
                ],
            )?;
        }
        Ok(())
    }

    pub fn get_plan(&self, id: &str) -> Result<Option<MealPlan>> {
        let meta = self
            .conn
            .query_row(
                "SELECT id, days, meals_per_day, rationale FROM meal_plans WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;

        let Some((id, days, mpd, rationale)) = meta else {
            return Ok(None);
        };

        let mut stmt = self.conn.prepare(
            "SELECT day, meal, recipe_id, recipe_title FROM plan_meals
             WHERE plan_id = ?1 ORDER BY day, meal",
        )?;
        let meals = stmt
            .query_map(params![id], |row| {
                Ok(PlannedMeal {
                    day: row.get::<_, i64>(0)? as u32,
                    meal: row.get::<_, i64>(1)? as u32,
                    recipe_id: RecipeId(row.get(2)?),
                    recipe_title: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(Some(MealPlan {
            id,
            days: days as u32,
            meals_per_day: mpd as u32,
            meals,
            rationale,
        }))
    }

    pub fn list_plans(&self) -> Result<Vec<MealPlan>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM meal_plans ORDER BY created_at DESC")?;
        let ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(p) = self.get_plan(&id)? {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Aggregate ingredient requirements for a set of recipes (sum canonical quantities).
    pub fn aggregate_ingredients(
        &self,
        recipe_ids: &[RecipeId],
    ) -> Result<Vec<(IngredientKey, f64)>> {
        use std::collections::HashMap;
        let mut map: HashMap<IngredientKey, f64> = HashMap::new();
        for rid in recipe_ids {
            let recipe = self
                .get_recipe(rid.as_str())?
                .with_context(|| format!("recipe {} not found", rid))?;
            for line in &recipe.ingredients {
                let key = IngredientKey::from_line(line);
                if let Some((canon, _)) = line.canonical_quantity() {
                    *map.entry(key).or_insert(0.0) += canon;
                } else {
                    // No quantity: still register presence with 0 so planners can see overlap.
                    map.entry(key).or_insert(0.0);
                }
            }
        }
        let mut v: Vec<_> = map.into_iter().collect();
        v.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        Ok(v)
    }

    /// Record (or refresh) a URL that failed to scrape, so future runs can skip it.
    pub fn record_scrape_failure(&self, url: &str, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scrape_failures (url, reason) VALUES (?1, ?2)
             ON CONFLICT(url) DO UPDATE SET reason=excluded.reason, failed_at=datetime('now')",
            params![url, reason],
        )?;
        Ok(())
    }

    /// URLs recorded as previously failed.
    pub fn failed_scrape_urls(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT url FROM scrape_failures")?;
        let urls = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<std::collections::HashSet<_>, _>>()?;
        Ok(urls)
    }

    /// Forget a recorded failure (e.g. once the URL scrapes successfully).
    pub fn clear_scrape_failure(&self, url: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM scrape_failures WHERE url = ?1", params![url])?;
        Ok(())
    }

    /// Find a recipe id whose source URL matches `url` after [`normalize_url`].
    ///
    /// Both the argument and each stored source (via [`recipe_source_url`]) are
    /// normalized. Scans all recipes; returns the first match in `list_recipes` order.
    pub fn find_id_by_normalized_source_url(&self, url: &str) -> Result<Option<String>> {
        let target = normalize_url(url);
        for r in self.list_recipes(None)? {
            if let Some(src) = recipe_source_url(&r) {
                if normalize_url(&src) == target {
                    return Ok(Some(r.id.as_str().to_string()));
                }
            }
        }
        Ok(None)
    }

    /// Find a recipe id whose title normalizes to `title_key` (caller should use
    /// [`normalize_title_key`]). Returns the first match in `list_recipes` order.
    pub fn find_id_by_title_key(&self, title_key: &str) -> Result<Option<String>> {
        for r in self.list_recipes(None)? {
            if normalize_title_key(&r.title) == title_key {
                return Ok(Some(r.id.as_str().to_string()));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::normalize_line;
    use tempfile::TempDir;

    fn sample_recipe(title: &str, lines: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = lines.iter().map(|l| normalize_line(l)).collect();
        r.steps = vec!["Cook it.".into()];
        r
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let r = sample_recipe("Pasta", &["500g pasta", "2 cups tomato sauce"]);
        let id = r.id.as_str().to_string();
        store.save_recipe(&r).unwrap();
        let loaded = store.get_recipe(&id).unwrap().unwrap();
        assert_eq!(loaded.title, "Pasta");
        assert_eq!(loaded.ingredients.len(), 2);
        assert_eq!(loaded.ingredients[0].name, "pasta");
    }

    #[test]
    fn ingredient_dedup_across_recipes() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let r1 = sample_recipe("A", &["1 cup milk"]);
        let r2 = sample_recipe("B", &["2 cups milk"]);
        store.save_recipe(&r1).unwrap();
        store.save_recipe(&r2).unwrap();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM ingredients WHERE name = 'milk'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn aggregate_sums_canonical() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let r1 = sample_recipe("A", &["1 cup milk"]);
        let r2 = sample_recipe("B", &["1 cup milk"]);
        store.save_recipe(&r1).unwrap();
        store.save_recipe(&r2).unwrap();
        let agg = store
            .aggregate_ingredients(&[r1.id.clone(), r2.id.clone()])
            .unwrap();
        let milk = agg.iter().find(|(k, _)| k.name == "milk").unwrap();
        // 2 cups → ~473.176 ml
        assert!((milk.1 - 473.176).abs() < 0.1);
    }

    #[test]
    fn scrape_failures_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        store
            .record_scrape_failure("https://x.com/a", "no recipe")
            .unwrap();
        store
            .record_scrape_failure("https://x.com/b", "http 404")
            .unwrap();
        let failed = store.failed_scrape_urls().unwrap();
        assert!(failed.contains("https://x.com/a"));
        assert_eq!(failed.len(), 2);

        store.clear_scrape_failure("https://x.com/a").unwrap();
        let failed = store.failed_scrape_urls().unwrap();
        assert!(!failed.contains("https://x.com/a"));
        assert_eq!(failed.len(), 1);
    }

    #[test]
    fn find_by_source_url_and_title_key() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let mut r = Recipe::new("Grilled S'mores");
        r.source = RecipeSource::Url {
            url: "https://example.com/grilled-smores".into(),
        };
        r.meta.source_url = Some("https://example.com/grilled-smores".into());
        store.save_recipe(&r).unwrap();

        let by_url = store
            .find_id_by_normalized_source_url("https://example.com/grilled-smores/")
            .unwrap();
        assert_eq!(by_url.as_deref(), Some(r.id.as_str()));

        let by_title = store
            .find_id_by_title_key(&normalize_title_key("GRILLED S'MORES"))
            .unwrap();
        assert_eq!(by_title.as_deref(), Some(r.id.as_str()));

        assert!(store
            .find_id_by_title_key(&normalize_title_key("Other"))
            .unwrap()
            .is_none());
    }
}
