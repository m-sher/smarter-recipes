//! Nutrition estimation: embedded per-100 g table, cached network lookups, and
//! per-recipe / per-plan macro totals with explicit coverage reporting.
//!
//! Estimates convert each ingredient line to grams (mass directly; volume via
//! the density table, defaulting to 1 g/ml for density-unknown liquids that
//! have a macro profile; count via per-item weights), then scale the per-100 g
//! profile. Ingredients that cannot be converted or have no profile are
//! reported as uncovered rather than silently guessed.

mod table;

pub use table::{grams_per_each, per_100g, per_100g_exact};

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

/// Throttle between FoodData Central requests to avoid tripping burst limits.
const FDC_REQUEST_DELAY: Duration = Duration::from_millis(250);
/// How many times to retry a single request that returns HTTP 429.
const FDC_MAX_RETRIES: u32 = 3;

/// Returned when FoodData Central keeps replying HTTP 429 after retries, so the
/// caller can stop the batch instead of burning the remaining quota.
#[derive(Debug)]
pub struct RateLimited {
    pub using_demo_key: bool,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.using_demo_key {
            write!(
                f,
                "FoodData Central rate limit (HTTP 429). The shared DEMO_KEY is heavily throttled; \
                 set SMARTER_RECIPES_FDC_KEY to a free key from https://api.data.gov/signup/ or retry later"
            )
        } else {
            write!(
                f,
                "FoodData Central rate limit (HTTP 429); slow down or retry later"
            )
        }
    }
}

impl std::error::Error for RateLimited {}

/// USDA FoodData Central search API. Uses `SMARTER_RECIPES_FDC_KEY` when set,
/// else the public `DEMO_KEY` (rate-limited; fine for occasional fetches).
pub struct FdcSource {
    client: reqwest::blocking::Client,
    api_key: String,
    pub base_url: String,
    /// Delay applied before each network request (0 to disable).
    pub request_delay: Duration,
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
            request_delay: FDC_REQUEST_DELAY,
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
            self.fetch_body(ingredient)?
        };
        // A 2xx body that is not FDC JSON (captive portal, gateway HTML) is an
        // error, not a definitive "no match" — otherwise the caller would
        // negative-cache a transient failure.
        let v: serde_json::Value =
            serde_json::from_str(&body).context("FoodData Central response was not JSON")?;
        Ok(parse_fdc_search(&v))
    }
}

impl FdcSource {
    /// Perform one search request, throttled and retrying HTTP 429 with backoff.
    /// A 429 that survives [`FDC_MAX_RETRIES`] surfaces as [`RateLimited`] so the
    /// caller can stop rather than burn the remaining quota. HTML entities in the
    /// stored name are decoded so junk like `salt&amp;pepper` still queries well.
    fn fetch_body(&self, ingredient: &str) -> Result<String> {
        let query = crate::net::decode_html_entities(ingredient);
        let url = format!(
            "{}/fdc/v1/foods/search?api_key={}&query={}&dataType=Foundation,SR%20Legacy&pageSize=25",
            self.base_url.trim_end_matches('/'),
            self.api_key,
            crate::net::encode_query(&query)
        );
        if self.request_delay > Duration::ZERO {
            std::thread::sleep(self.request_delay);
        }
        let mut attempt = 0u32;
        loop {
            let resp = self
                .client
                .get(&url)
                .send()
                .context("FoodData Central request")?;
            let status = resp.status();
            if status.as_u16() == 429 {
                if attempt >= FDC_MAX_RETRIES {
                    return Err(anyhow::Error::new(RateLimited {
                        using_demo_key: self.api_key == "DEMO_KEY",
                    }));
                }
                let wait = retry_after(resp.headers()).unwrap_or_else(|| backoff(attempt));
                std::thread::sleep(wait);
                attempt += 1;
                continue;
            }
            if !status.is_success() {
                bail!("FoodData Central HTTP {status}");
            }
            return resp.text().context("reading FoodData Central body");
        }
    }
}

/// Retry delay from a `Retry-After` header (seconds form), capped at 60s.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    parse_retry_after_secs(raw)
}

fn parse_retry_after_secs(raw: &str) -> Option<Duration> {
    let secs: u64 = raw.trim().parse().ok()?;
    Some(Duration::from_secs(secs.min(60)))
}

/// Exponential backoff for retry `attempt` (0-based): 0.5s, 1s, 2s, … capped 8s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis((500u64 << attempt.min(4)).min(8_000))
}

/// True for names not worth an FDC lookup: empty, or HTML-entity leftovers
/// (e.g. `&nbsp`) that slipped past ingest cleaning in already-stored recipes.
pub fn is_probable_junk_name(name: &str) -> bool {
    let t = name.trim();
    t.is_empty() || t.starts_with('&') || !t.chars().any(|c| c.is_alphabetic())
}

/// Extract per-100 g macros from an FDC search response, preferring the food
/// whose description best matches how recipes measure ingredients: raw for
/// produce/meat, dry for grains/pasta/legumes. Without this, FDC's relevance
/// order often returns a cooked/canned entry (e.g. cooked quinoa at ~120
/// kcal/100 g vs ~368 dry), understating totals several-fold.
fn parse_fdc_search(v: &serde_json::Value) -> Option<Macros> {
    let foods = v.get("foods").and_then(|f| f.as_array())?;
    let mut best: Option<(i32, Macros)> = None;
    for food in foods {
        let Some(macros) = macros_from_food(food) else {
            continue;
        };
        let desc = food
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_lowercase();
        let dtype = food
            .get("dataType")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_lowercase();
        let score = fdc_food_score(&desc, &dtype);
        if best.as_ref().is_none_or(|(bs, _)| score > *bs) {
            best = Some((score, macros));
        }
    }
    best.map(|(_, m)| m)
}

/// Rank an FDC food by how well its preparation state matches recipe usage.
/// Prep state dominates; data source is a light tiebreaker.
fn fdc_food_score(desc: &str, dtype: &str) -> i32 {
    let mut score = 0;
    const PREPARED: &[&str] = &[
        "cooked", "boiled", "roasted", "baked", "fried", "braised", "grilled", "steamed",
        "prepared", "canned", "drained", "frozen", "moist", "reheated",
    ];
    const UNPREPARED: &[&str] = &["raw", "uncooked", "dry", "dried"];
    if PREPARED.iter().any(|w| desc.contains(w)) {
        score -= 10;
    }
    if UNPREPARED.iter().any(|w| desc.contains(w)) {
        score += 5;
    }
    // Prefer curated reference data over branded/survey rows.
    score += match dtype {
        "foundation" => 2,
        "sr legacy" => 1,
        _ => 0,
    };
    score
}

/// Per-100 g macros from one FDC food, or `None` if it has no kcal value.
fn macros_from_food(food: &serde_json::Value) -> Option<Macros> {
    let nutrients = food.get("foodNutrients").and_then(|n| n.as_array())?;
    let mut kcal = None;
    let mut protein = None;
    let mut fat = None;
    let mut carbs = None;
    for n in nutrients {
        let Some(value) = n.get("value").and_then(|x| x.as_f64()) else {
            continue;
        };
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
    kcal.map(|kcal| Macros {
        kcal,
        protein_g: protein.unwrap_or(0.0),
        fat_g: fat.unwrap_or(0.0),
        carbs_g: carbs.unwrap_or(0.0),
    })
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

/// Per-100 g profile for `name`, most-specific candidate first. For each
/// candidate the cache/overlay is consulted before the embedded table, so a
/// specific full-name table entry is never shadowed by a cached generic token.
pub fn resolve_profile(name: &str, extra: &HashMap<String, Macros>) -> Option<Macros> {
    for cand in name_candidates(name) {
        if let Some(m) = extra.get(&cand) {
            return Some(*m);
        }
        if let Some(m) = per_100g_exact(&cand) {
            return Some(m);
        }
    }
    None
}

/// Words that mark a Count as a container or whole unit of unknown size
/// ("2 cans tomatoes", "1 head garlic"), where multiplying a per-item weight
/// would be meaningless. Excludes portion words the per-item table is keyed on
/// (clove, stalk), so those still convert.
const CONTAINER_WORDS: &[&str] = &[
    "can",
    "cans",
    "jar",
    "jars",
    "carton",
    "cartons",
    "bottle",
    "bottles",
    "package",
    "packages",
    "packet",
    "packets",
    "bag",
    "bags",
    "box",
    "boxes",
    "tin",
    "tins",
    "container",
    "containers",
    "pack",
    "packs",
    "head",
    "heads",
    "bunch",
    "bunches",
    "loaf",
    "loaves",
];

/// Keywords marking an ingredient as a liquid, for which a density-unknown
/// volume can be estimated at ≈1 g/ml. Solids are intentionally excluded so
/// they are reported uncovered rather than silently mis-weighed.
const LIQUID_KEYWORDS: &[&str] = &[
    "water", "milk", "cream", "broth", "stock", "juice", "wine", "beer", "vinegar", "sauce", "oil",
    "syrup", "extract", "soda", "tea", "coffee", "liqueur", "brandy", "rum", "sherry", "mirin",
];

fn is_liquid_name(name: &str) -> bool {
    LIQUID_KEYWORDS.iter().any(|w| name.contains(w))
}

fn mentions_container(original: &str) -> bool {
    original
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|w| CONTAINER_WORDS.contains(&w))
}

/// Grams represented by one ingredient line, when convertible.
fn line_grams(line: &crate::domain::IngredientLine) -> Option<f64> {
    use crate::domain::UnitKind;
    let (qty, kind) = line.canonical_quantity()?;
    let key = IngredientKey::from_line(line);
    match kind {
        UnitKind::Mass => Some(qty),
        UnitKind::Volume => volume_ml_to_mass_g(&key.name, qty)
            // Density-unknown liquids (sauces, broths) are ≈1 g/ml; solids
            // without a density are left unconvertible (reported uncovered).
            .or_else(|| is_liquid_name(&key.name).then_some(qty)),
        // A bare count of whole items converts via per-item weight, but a count
        // of containers ("2 cans") has unknown size and is left unconvertible.
        UnitKind::Count if mentions_container(&line.original) => None,
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
    /// `(day index, macros)` for each day that has at least one meal, in day
    /// order. Sized by the meals present, not by the requested day count.
    pub per_day: Vec<(usize, Macros)>,
    pub total: Macros,
    /// Distinct ingredient names that contributed to the totals.
    pub covered: BTreeSet<String>,
    /// Distinct names never estimated in any recipe (excludes names covered
    /// elsewhere), so `covered` and `uncovered` are disjoint.
    pub uncovered: BTreeSet<String>,
    /// Subset of `uncovered` that `nutrition fetch` could resolve: no macro
    /// profile exists yet (as opposed to a profile that cannot be converted to
    /// grams, which fetching cannot fix).
    pub fetchable: BTreeSet<String>,
}

/// Estimate nutrition for every meal in a plan (whole recipes per day).
pub fn plan_nutrition(
    store: &Store,
    plan: &MealPlan,
    extra: &HashMap<String, Macros>,
) -> Result<PlanNutrition> {
    use std::collections::BTreeMap;
    let mut by_day: BTreeMap<usize, Macros> = BTreeMap::new();
    let mut total = Macros::default();
    let mut covered = BTreeSet::new();
    let mut uncovered_raw = BTreeSet::new();
    for meal in &plan.meals {
        let recipe = store
            .get_recipe(meal.recipe_id.as_str())?
            .with_context(|| format!("recipe {} missing", meal.recipe_id))?;
        let rn = recipe_nutrition(&recipe, extra);
        by_day.entry(meal.day as usize).or_default().add(&rn.macros);
        total.add(&rn.macros);
        covered.extend(rn.covered);
        uncovered_raw.extend(rn.uncovered);
    }
    // A name covered in any recipe counts as covered overall, so the two sets
    // are disjoint and the coverage denominator does not double-count.
    let uncovered: BTreeSet<String> = uncovered_raw.difference(&covered).cloned().collect();
    let fetchable = uncovered
        .iter()
        .filter(|n| resolve_profile(n, extra).is_none())
        .cloned()
        .collect();
    Ok(PlanNutrition {
        per_day: by_day.into_iter().collect(),
        total,
        covered,
        uncovered,
        fetchable,
    })
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
    fn volume_solid_without_density_is_uncovered_not_1g_per_ml() {
        // Broccoli has a macro profile but no density entry; a volume measure
        // must not be silently treated as 1 g/ml (which would inflate it).
        let r = rec("T", &["2 cups broccoli"]);
        let n = recipe_nutrition(&r, &HashMap::new());
        assert!(
            n.uncovered.contains("broccoli"),
            "solid should be uncovered, got covered={:?}",
            n.covered
        );
        assert_eq!(n.macros, Macros::default());
    }

    #[test]
    fn count_of_containers_is_uncovered() {
        // "2 cans tomatoes" must not be 2 × one-tomato weight.
        let r = rec("T", &["2 cans tomatoes"]);
        let n = recipe_nutrition(&r, &HashMap::new());
        assert!(n.uncovered.contains("tomatoes"));
        // A bare count of the same item still converts.
        let r2 = rec("T", &["2 tomatoes"]);
        let n2 = recipe_nutrition(&r2, &HashMap::new());
        assert!(n2.covered.contains("tomatoes"));
    }

    #[test]
    fn cached_generic_does_not_shadow_specific_builtin() {
        // A cached generic "oil" must not override the built-in "olive oil".
        let mut extra = HashMap::new();
        extra.insert(
            "oil".to_string(),
            Macros {
                kcal: 1.0,
                protein_g: 0.0,
                fat_g: 0.0,
                carbs_g: 0.0,
            },
        );
        let m = resolve_profile("olive oil", &extra).unwrap();
        assert!((m.kcal - 884.0).abs() < 1.0, "kcal = {}", m.kcal);
        // But the generic still applies to a name with no specific entry.
        assert_eq!(resolve_profile("truffle oil", &extra).unwrap().kcal, 1.0);
    }

    fn fdc(body: &str) -> Option<Macros> {
        parse_fdc_search(&serde_json::from_str(body).unwrap())
    }

    #[test]
    fn fdc_parse_by_number_and_name() {
        let body = r#"{"foods":[{"dataType":"SR Legacy","foodNutrients":[
            {"nutrientNumber":"208","nutrientName":"Energy","unitName":"KCAL","value":364.0},
            {"nutrientNumber":"203","nutrientName":"Protein","unitName":"G","value":10.3},
            {"nutrientNumber":"204","nutrientName":"Total lipid (fat)","unitName":"G","value":1.0},
            {"nutrientNumber":"205","nutrientName":"Carbohydrate, by difference","unitName":"G","value":76.3}
        ]}]}"#;
        let m = fdc(body).unwrap();
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
        let m2 = fdc(body2).unwrap();
        assert!((m2.kcal - 52.0).abs() < 1e-9);
        assert!(fdc(r#"{"foods":[]}"#).is_none());
    }

    #[test]
    fn fdc_prefers_raw_dry_over_cooked() {
        // FDC relevance order puts the cooked entry first; we must pick dry.
        let body = r#"{"foods":[
            {"description":"Quinoa, cooked","dataType":"Survey (FNDDS)","foodNutrients":[
                {"nutrientNumber":"208","unitName":"KCAL","value":120.0}]},
            {"description":"Quinoa, uncooked","dataType":"SR Legacy","foodNutrients":[
                {"nutrientNumber":"208","unitName":"KCAL","value":368.0}]}
        ]}"#;
        assert!((fdc(body).unwrap().kcal - 368.0).abs() < 1e-9);

        // Raw beats canned/drained for produce, too.
        let body2 = r#"{"foods":[
            {"description":"Beans, canned, drained","dataType":"SR Legacy","foodNutrients":[
                {"nutrientNumber":"208","unitName":"KCAL","value":91.0}]},
            {"description":"Beans, raw","dataType":"SR Legacy","foodNutrients":[
                {"nutrientNumber":"208","unitName":"KCAL","value":333.0}]}
        ]}"#;
        assert!((fdc(body2).unwrap().kcal - 333.0).abs() < 1e-9);
    }

    #[test]
    fn fdc_non_json_body_is_error_not_miss() {
        let src = FdcSource {
            offline_body: Some("<html>maintenance</html>".into()),
            ..Default::default()
        };
        // Must be Err (so the caller does not negative-cache), not Ok(None).
        assert!(src.lookup("flour").is_err());
    }

    #[test]
    fn fdc_valid_json_no_match_is_ok_none() {
        let src = FdcSource {
            offline_body: Some(r#"{"foods":[]}"#.into()),
            ..Default::default()
        };
        assert!(src.lookup("flour").unwrap().is_none());
    }

    #[test]
    fn parse_retry_after_reads_seconds_and_caps() {
        assert_eq!(parse_retry_after_secs("5"), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after_secs(" 12 "),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            parse_retry_after_secs("100000"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            parse_retry_after_secs("Wed, 21 Oct 2015 07:28:00 GMT"),
            None
        );
        assert_eq!(parse_retry_after_secs("nonsense"), None);
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_millis(1000));
        assert_eq!(backoff(2), Duration::from_millis(2000));
        assert!(backoff(10) <= Duration::from_millis(8000));
    }

    #[test]
    fn junk_names_are_skipped() {
        assert!(is_probable_junk_name("&nbsp"));
        assert!(is_probable_junk_name(""));
        assert!(is_probable_junk_name("   "));
        assert!(is_probable_junk_name("1/2"));
        assert!(!is_probable_junk_name("olive oil"));
        assert!(!is_probable_junk_name("(1 oz) taco seasoning"));
    }

    #[test]
    fn plan_nutrition_per_day_coverage_and_fetchable() {
        use crate::domain::{MealPlan, PlannedMeal, RecipeId};
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();

        let mut day0 = rec("Day0", &["100 g flour"]); // covered
        day0.id = RecipeId::from("r0");
        let mut day2 = rec("Day2", &["3 dragonfruit"]); // uncovered, no profile
        day2.id = RecipeId::from("r2");
        store.save_recipe(&day0).unwrap();
        store.save_recipe(&day2).unwrap();

        // Meals on day 0 and day 2 (day 1 empty) — must not allocate by days.
        let plan = MealPlan {
            id: "p".into(),
            days: 3,
            meals_per_day: 1,
            rationale: String::new(),
            meals: vec![
                PlannedMeal {
                    day: 0,
                    meal: 0,
                    recipe_id: RecipeId::from("r0"),
                    recipe_title: "Day0".into(),
                },
                PlannedMeal {
                    day: 2,
                    meal: 0,
                    recipe_id: RecipeId::from("r2"),
                    recipe_title: "Day2".into(),
                },
            ],
        };
        let pn = plan_nutrition(&store, &plan, &HashMap::new()).unwrap();
        // Only days with meals appear, keyed by real day index.
        assert_eq!(pn.per_day.len(), 2);
        assert_eq!(pn.per_day[0].0, 0);
        assert_eq!(pn.per_day[1].0, 2);
        assert!((pn.per_day[0].1.kcal - 364.0).abs() < 1.0);
        assert!(pn.covered.contains("flour"));
        assert!(pn.uncovered.contains("dragonfruit"));
        // No profile for dragonfruit -> fetchable.
        assert!(pn.fetchable.contains("dragonfruit"));
    }

    #[test]
    fn plan_coverage_sets_are_disjoint() {
        use crate::domain::{MealPlan, PlannedMeal, RecipeId};
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        // Same name covered in one recipe (mass) and uncovered in another
        // (volume solid) must count once, as covered.
        let mut a = rec("A", &["100 g broccoli"]);
        a.id = RecipeId::from("a");
        let mut b = rec("B", &["2 cups broccoli"]);
        b.id = RecipeId::from("b");
        store.save_recipe(&a).unwrap();
        store.save_recipe(&b).unwrap();
        let plan = MealPlan {
            id: "p".into(),
            days: 2,
            meals_per_day: 1,
            rationale: String::new(),
            meals: vec![
                PlannedMeal {
                    day: 0,
                    meal: 0,
                    recipe_id: RecipeId::from("a"),
                    recipe_title: "A".into(),
                },
                PlannedMeal {
                    day: 1,
                    meal: 0,
                    recipe_id: RecipeId::from("b"),
                    recipe_title: "B".into(),
                },
            ],
        };
        let pn = plan_nutrition(&store, &plan, &HashMap::new()).unwrap();
        assert!(pn.covered.contains("broccoli"));
        assert!(!pn.uncovered.contains("broccoli"));
        // Not fetchable: a profile exists, it just wasn't convertible in B.
        assert!(!pn.fetchable.contains("broccoli"));
    }
}
