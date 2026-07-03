//! Nutrition estimation: embedded per-100 g table, cached network lookups, and
//! per-recipe / per-plan macro totals with explicit coverage reporting.
//!
//! Estimates convert each ingredient line to grams (mass directly; volume via
//! the density table, defaulting to 1 g/ml for density-unknown liquids that
//! have a macro profile; count via per-item weights), then scale the per-100 g
//! profile. Ingredients that cannot be converted or have no profile are
//! reported as uncovered rather than silently guessed.

mod table;

pub use table::{grams_per_each, per_100g};

use crate::domain::{name_candidates, IngredientKey, Macros, MealPlan, Recipe};
use crate::pricing::volume_ml_to_mass_g;
use crate::storage::Store;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

/// Looks up a per-100 g macro profile for an ingredient name.
pub trait NutritionSource {
    fn name(&self) -> &'static str;
    fn lookup(&self, ingredient: &str) -> Result<Option<Macros>>;
}

/// USDA FoodData Central search API. Uses `SMARTER_RECIPES_FDC_KEY` when set,
/// else the public `DEMO_KEY` (rate-limited; fine for occasional fetches).
pub struct FdcSource {
    client: reqwest::blocking::Client,
    api_key: String,
    pub base_url: String,
    /// Canned response body for offline tests.
    pub offline_body: Option<String>,
}

impl Default for FdcSource {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("smarter-recipes/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build blocking HTTP client");
        Self {
            client,
            api_key: std::env::var("SMARTER_RECIPES_FDC_KEY").unwrap_or_else(|_| "DEMO_KEY".into()),
            base_url: "https://api.nal.usda.gov".into(),
            offline_body: None,
        }
    }
}

impl NutritionSource for FdcSource {
    fn name(&self) -> &'static str {
        "fdc"
    }

    fn lookup(&self, ingredient: &str) -> Result<Option<Macros>> {
        let body = if let Some(ref b) = self.offline_body {
            b.clone()
        } else {
            let url = format!(
                "{}/fdc/v1/foods/search?api_key={}&query={}&dataType=Foundation,SR%20Legacy&pageSize=10",
                self.base_url.trim_end_matches('/'),
                self.api_key,
                urlencode(ingredient)
            );
            let resp = self
                .client
                .get(&url)
                .send()
                .context("FoodData Central request")?;
            if !resp.status().is_success() {
                bail!("FoodData Central HTTP {}", resp.status());
            }
            resp.text()?
        };
        Ok(parse_fdc_search(&body))
    }
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => c.to_string(),
            c => {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf)
                    .bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect()
            }
        })
        .collect()
}

/// Extract per-100 g macros from an FDC search response. Takes the first food
/// with a usable Energy (kcal) value; nutrients matched by number or name.
fn parse_fdc_search(body: &str) -> Option<Macros> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let foods = v.get("foods")?.as_array()?;
    for food in foods {
        let Some(nutrients) = food.get("foodNutrients").and_then(|n| n.as_array()) else {
            continue;
        };
        let mut kcal = None;
        let mut protein = None;
        let mut fat = None;
        let mut carbs = None;
        for n in nutrients {
            let value = n.get("value").and_then(|x| x.as_f64());
            let Some(value) = value else { continue };
            let number = n
                .get("nutrientNumber")
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| x.to_string())
                })
                .unwrap_or_default();
            let name = n
                .get("nutrientName")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_lowercase();
            let unit = n
                .get("unitName")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_uppercase();
            match number.as_str() {
                "208" => {
                    if unit == "KCAL" {
                        kcal = Some(value);
                    }
                }
                "203" => protein = Some(value),
                "204" => fat = Some(value),
                "205" => carbs = Some(value),
                _ => {
                    if kcal.is_none() && name.starts_with("energy") && unit == "KCAL" {
                        kcal = Some(value);
                    } else if protein.is_none() && name == "protein" {
                        protein = Some(value);
                    } else if fat.is_none() && name.starts_with("total lipid") {
                        fat = Some(value);
                    } else if carbs.is_none() && name.starts_with("carbohydrate") {
                        carbs = Some(value);
                    }
                }
            }
        }
        if let Some(kcal) = kcal {
            return Some(Macros {
                kcal,
                protein_g: protein.unwrap_or(0.0),
                fat_g: fat.unwrap_or(0.0),
                carbs_g: carbs.unwrap_or(0.0),
            });
        }
    }
    None
}

/// Offline source backed by a JSON map: `{ "name": {"kcal":..,"protein_g":..,
/// "fat_g":..,"carbs_g":..}, ... }`.
pub struct FixtureNutritionSource {
    map: HashMap<String, Macros>,
}

impl FixtureNutritionSource {
    pub fn from_path(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading nutrition fixture {}", path.display()))?;
        let map: HashMap<String, Macros> =
            serde_json::from_str(&text).context("parsing nutrition fixture JSON")?;
        Ok(Self {
            map: map
                .into_iter()
                .map(|(k, v)| (k.to_lowercase(), v))
                .collect(),
        })
    }
}

impl NutritionSource for FixtureNutritionSource {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn lookup(&self, ingredient: &str) -> Result<Option<Macros>> {
        for cand in name_candidates(ingredient) {
            if let Some(m) = self.map.get(&cand) {
                return Ok(Some(*m));
            }
        }
        Ok(None)
    }
}

/// Per-100 g profile for `name`: cache/overlay entries first, then the
/// embedded table.
pub fn resolve_profile(name: &str, extra: &HashMap<String, Macros>) -> Option<Macros> {
    for cand in name_candidates(name) {
        if let Some(m) = extra.get(&cand) {
            return Some(*m);
        }
    }
    per_100g(name)
}

/// Grams represented by one ingredient line, when convertible.
fn line_grams(line: &crate::domain::IngredientLine) -> Option<f64> {
    use crate::domain::UnitKind;
    let (qty, kind) = line.canonical_quantity()?;
    let key = IngredientKey::from_line(line);
    match kind {
        UnitKind::Mass => Some(qty),
        UnitKind::Volume => volume_ml_to_mass_g(&key.name, qty)
            // Density-unknown liquids (sauces, broths) are ≈1 g/ml; only used
            // when a macro profile exists, and always labeled an estimate.
            .or(Some(qty)),
        UnitKind::Count => grams_per_each(&key.name).map(|g| g * qty),
        UnitKind::Other => None,
    }
}

/// Computed macro totals plus coverage for one recipe (whole recipe, not per
/// serving).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecipeNutrition {
    pub macros: Macros,
    /// Distinct ingredient names that contributed to the totals.
    pub covered: BTreeSet<String>,
    /// Distinct ingredient names that could not be estimated.
    pub uncovered: BTreeSet<String>,
}

/// Estimate whole-recipe macros. Lines with no parsed quantity (e.g. "to
/// taste") contribute nothing and do not count against coverage.
pub fn recipe_nutrition(recipe: &Recipe, extra: &HashMap<String, Macros>) -> RecipeNutrition {
    let mut out = RecipeNutrition::default();
    for line in &recipe.ingredients {
        if line.canonical_quantity().is_none() {
            continue;
        }
        let key = IngredientKey::from_line(line);
        match (resolve_profile(&key.name, extra), line_grams(line)) {
            (Some(profile), Some(grams)) => {
                out.macros.add_scaled(&profile, grams);
                out.covered.insert(key.name);
            }
            _ => {
                out.uncovered.insert(key.name);
            }
        }
    }
    out
}

/// Per-day and total macro estimates for a plan, with coverage.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanNutrition {
    /// Indexed by plan day; days without meals stay zero.
    pub per_day: Vec<Macros>,
    pub total: Macros,
    pub covered: BTreeSet<String>,
    pub uncovered: BTreeSet<String>,
}

/// Estimate nutrition for every meal in a plan (whole recipes per day).
pub fn plan_nutrition(
    store: &Store,
    plan: &MealPlan,
    extra: &HashMap<String, Macros>,
) -> Result<PlanNutrition> {
    let mut out = PlanNutrition {
        per_day: vec![Macros::default(); plan.days.max(1) as usize],
        ..Default::default()
    };
    for meal in &plan.meals {
        let recipe = store
            .get_recipe(meal.recipe_id.as_str())?
            .with_context(|| format!("recipe {} missing", meal.recipe_id))?;
        let rn = recipe_nutrition(&recipe, extra);
        if let Some(day) = out.per_day.get_mut(meal.day as usize) {
            day.add(&rn.macros);
        }
        out.total.add(&rn.macros);
        out.covered.extend(rn.covered);
        out.uncovered.extend(rn.uncovered);
    }
    // A name covered in one recipe but uncovered in another (different unit
    // kind) counts as partially covered; keep it in both sets out of honesty —
    // coverage is reported as covered/(covered+uncovered).
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::normalize_line;

    fn rec(title: &str, ings: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    #[test]
    fn recipe_macros_mass_volume_count() {
        // 100 g flour (364 kcal) + 2 eggs (100 g -> 143 kcal) + 1 tbsp olive oil
        // (~14.8 ml * 0.91 g/ml = ~13.5 g -> ~119 kcal)
        let r = rec("T", &["100 g flour", "2 eggs", "1 tbsp olive oil"]);
        let n = recipe_nutrition(&r, &HashMap::new());
        assert!(n.uncovered.is_empty(), "uncovered: {:?}", n.uncovered);
        assert!(
            (n.macros.kcal - (364.0 + 143.0 + 119.0)).abs() < 15.0,
            "kcal = {}",
            n.macros.kcal
        );
    }

    #[test]
    fn unknown_ingredient_reported_uncovered() {
        let r = rec("T", &["100 g unobtainium"]);
        let n = recipe_nutrition(&r, &HashMap::new());
        assert_eq!(n.macros, Macros::default());
        assert!(n.uncovered.contains("unobtainium"));
    }

    #[test]
    fn to_taste_lines_ignored_for_coverage() {
        let r = rec("T", &["salt to taste"]);
        let n = recipe_nutrition(&r, &HashMap::new());
        assert!(n.covered.is_empty() && n.uncovered.is_empty());
    }

    #[test]
    fn extra_profiles_override_and_extend() {
        let mut extra = HashMap::new();
        extra.insert(
            "unobtainium".to_string(),
            Macros {
                kcal: 500.0,
                protein_g: 1.0,
                fat_g: 2.0,
                carbs_g: 3.0,
            },
        );
        let r = rec("T", &["200 g unobtainium"]);
        let n = recipe_nutrition(&r, &extra);
        assert!(n.uncovered.is_empty());
        assert!((n.macros.kcal - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn density_unknown_liquid_estimated_at_1g_per_ml() {
        // soy sauce has a macro profile but no density entry: 100 ml ≈ 100 g.
        let r = rec("T", &["100 ml soy sauce"]);
        let n = recipe_nutrition(&r, &HashMap::new());
        assert!(n.uncovered.is_empty());
        assert!((n.macros.kcal - 53.0).abs() < 1.0);
    }

    #[test]
    fn count_without_each_weight_uncovered() {
        let r = rec("T", &["2 mystery pods"]);
        let n = recipe_nutrition(&r, &HashMap::new());
        assert!(n.uncovered.contains("mystery pods"));
    }

    #[test]
    fn fdc_parse_by_number_and_name() {
        let body = r#"{"foods":[{"dataType":"SR Legacy","foodNutrients":[
            {"nutrientNumber":"208","nutrientName":"Energy","unitName":"KCAL","value":364.0},
            {"nutrientNumber":"203","nutrientName":"Protein","unitName":"G","value":10.3},
            {"nutrientNumber":"204","nutrientName":"Total lipid (fat)","unitName":"G","value":1.0},
            {"nutrientNumber":"205","nutrientName":"Carbohydrate, by difference","unitName":"G","value":76.3}
        ]}]}"#;
        let m = parse_fdc_search(body).unwrap();
        assert!((m.kcal - 364.0).abs() < 1e-9);
        assert!((m.carbs_g - 76.3).abs() < 1e-9);

        // Name-based fallback when numbers are absent; kJ-only foods skipped.
        let body2 = r#"{"foods":[
            {"foodNutrients":[{"nutrientName":"Energy","unitName":"kJ","value":1523.0}]},
            {"foodNutrients":[
                {"nutrientName":"Energy","unitName":"KCAL","value":52.0},
                {"nutrientName":"Protein","unitName":"G","value":0.3}
            ]}
        ]}"#;
        let m2 = parse_fdc_search(body2).unwrap();
        assert!((m2.kcal - 52.0).abs() < 1e-9);
        assert!(parse_fdc_search(r#"{"foods":[]}"#).is_none());
    }
}
