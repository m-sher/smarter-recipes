//! Nutrition min/max bounds for steering meal plans.

use crate::domain::Macros;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// One optional min/max pair for a single macro nutrient.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MacroRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl MacroRange {
    pub fn is_empty(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    fn validate(&self, label: &str) -> Result<()> {
        if let (Some(lo), Some(hi)) = (self.min, self.max) {
            if lo > hi {
                bail!("{label}: min ({lo}) is greater than max ({hi})");
            }
        }
        if let Some(lo) = self.min {
            if !lo.is_finite() {
                bail!("{label}: min must be finite");
            }
        }
        if let Some(hi) = self.max {
            if !hi.is_finite() {
                bail!("{label}: max must be finite");
            }
        }
        Ok(())
    }
}

/// Default tolerance (percentage points) for a macro-ratio target when the
/// config omits `tolerance`.
pub const DEFAULT_RATIO_TOLERANCE: f64 = 5.0;

/// Target macro split for a scope, as a percentage of total macro grams
/// (`protein_g + fat_g + carbs_g`). Each share is optional and independent; a
/// share is satisfied when the actual share is within `tolerance` percentage
/// points of the target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MacroRatio {
    pub protein: Option<f64>,
    pub fat: Option<f64>,
    pub carb: Option<f64>,
    /// Allowed deviation in percentage points (default [`DEFAULT_RATIO_TOLERANCE`]).
    pub tolerance: Option<f64>,
}

impl MacroRatio {
    pub fn is_empty(&self) -> bool {
        self.protein.is_none() && self.fat.is_none() && self.carb.is_none()
    }

    /// Tolerance in percentage points, defaulting when unset.
    pub fn effective_tolerance(&self) -> f64 {
        self.tolerance.unwrap_or(DEFAULT_RATIO_TOLERANCE)
    }

    fn validate(&self, scope: &str) -> Result<()> {
        for (name, share) in [
            ("protein", self.protein),
            ("fat", self.fat),
            ("carb", self.carb),
        ] {
            if let Some(s) = share {
                if !s.is_finite() || !(0.0..=100.0).contains(&s) {
                    bail!("{scope}.ratio.{name}: share must be a finite percentage in [0, 100]");
                }
            }
        }
        if let Some(t) = self.tolerance {
            if !t.is_finite() || t < 0.0 {
                bail!("{scope}.ratio.tolerance: must be a finite, non-negative percentage");
            }
        }
        Ok(())
    }
}

/// Normalized comparison key for a single category token (case-insensitive,
/// trimmed, whitespace-collapsed).
fn category_key(s: &str) -> String {
    crate::domain::normalize_title_key(s)
}

/// Expand a config list into normalized match tokens. Each entry may itself be
/// comma-joined (e.g. `"Main Course, Sauce"`).
fn expand_tokens(list: &[String]) -> Vec<String> {
    list.iter()
        .flat_map(|e| e.split(','))
        .map(category_key)
        .filter(|t| !t.is_empty())
        .collect()
}

/// Category-based pool filter: keep standalone meals, drop components (sauces,
/// dressings, condiments) by their schema.org `recipeCategory`
/// ([`crate::domain::RecipeMeta::category`]).
///
/// A recipe's stored category may list several values joined by ", "; a recipe
/// matches a list when any of its tokens equals a list entry (compared on the
/// normalized key). Whenever either list is non-empty, recipes with **no**
/// category tokens are excluded (same strictness as a non-empty whitelist).
/// Not counted by [`NutritionBounds::is_empty`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CategoryFilter {
    /// When non-empty, ONLY recipes whose category matches one of these are
    /// eligible. Uncategorized recipes are excluded whenever any filter list
    /// is configured (see [`Self::allows`]).
    #[serde(default)]
    pub whitelist: Vec<String>,
    /// Recipes whose category matches one of these are always excluded.
    /// Takes precedence over the whitelist. A non-empty blacklist alone also
    /// excludes uncategorized recipes.
    #[serde(default)]
    pub blacklist: Vec<String>,
}

/// Why a recipe was excluded by a non-empty category filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryDrop {
    /// No category tokens (missing / blank). Suggest re-running `categorize`.
    NoCategory,
    /// Has tokens but failed blacklist and/or whitelist.
    Filtered,
}

impl CategoryFilter {
    pub fn is_empty(&self) -> bool {
        self.whitelist.is_empty() && self.blacklist.is_empty()
    }

    fn validate(&self) -> Result<()> {
        for (list, label) in [
            (&self.whitelist, "whitelist"),
            (&self.blacklist, "blacklist"),
        ] {
            if list
                .iter()
                .any(|e| e.split(',').all(|t| category_key(t).is_empty()))
            {
                bail!("category.{label}: entries must be non-empty");
            }
        }
        // Compare at the token level.
        let black = expand_tokens(&self.blacklist);
        if let Some(dup) = expand_tokens(&self.whitelist)
            .iter()
            .find(|t| black.contains(t))
        {
            bail!("category: '{dup}' appears in both whitelist and blacklist");
        }
        Ok(())
    }

    /// Whether a recipe with this (optional, possibly comma-joined) category is
    /// eligible for the meal pool.
    ///
    /// - Empty filter → everything is allowed.
    /// - Any non-empty blacklist and/or whitelist → recipes with no category
    ///   tokens are excluded.
    /// - Blacklist wins: any black token drops the recipe.
    /// - Non-empty whitelist then requires at least one white token.
    pub fn allows(&self, category: Option<&str>) -> bool {
        self.drop_reason(category).is_none()
    }

    /// `None` if allowed; otherwise why it was excluded.
    pub fn drop_reason(&self, category: Option<&str>) -> Option<CategoryDrop> {
        if self.is_empty() {
            return None;
        }
        let tokens: Vec<String> = category
            .into_iter()
            .flat_map(|c| c.split(','))
            .map(category_key)
            .filter(|t| !t.is_empty())
            .collect();
        // Configured filter is strict: no category means we can't classify it.
        if tokens.is_empty() {
            return Some(CategoryDrop::NoCategory);
        }
        let black = expand_tokens(&self.blacklist);
        if tokens.iter().any(|t| black.contains(t)) {
            return Some(CategoryDrop::Filtered);
        }
        if !self.whitelist.is_empty() {
            let white = expand_tokens(&self.whitelist);
            if !tokens.iter().any(|t| white.contains(t)) {
                return Some(CategoryDrop::Filtered);
            }
        }
        None
    }
}

/// Min/max ranges for all tracked macros, plus an optional target macro split.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MacroBounds {
    #[serde(default)]
    pub kcal: MacroRange,
    #[serde(default)]
    pub protein_g: MacroRange,
    #[serde(default)]
    pub fat_g: MacroRange,
    #[serde(default)]
    pub carbs_g: MacroRange,
    #[serde(default)]
    pub ratio: MacroRatio,
}

impl MacroBounds {
    pub fn is_empty(&self) -> bool {
        self.kcal.is_empty()
            && self.protein_g.is_empty()
            && self.fat_g.is_empty()
            && self.carbs_g.is_empty()
            && self.ratio.is_empty()
    }

    pub fn validate(&self, scope: &str) -> Result<()> {
        self.kcal.validate(&format!("{scope}.kcal"))?;
        self.protein_g.validate(&format!("{scope}.protein_g"))?;
        self.fat_g.validate(&format!("{scope}.fat_g"))?;
        self.carbs_g.validate(&format!("{scope}.carbs_g"))?;
        self.ratio.validate(scope)?;
        Ok(())
    }
}

/// Full constraint set: per-day, per-meal, and whole-plan scopes, plus an
/// optional category-based pool filter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NutritionBounds {
    #[serde(default)]
    pub per_day: MacroBounds,
    #[serde(default)]
    pub per_meal: MacroBounds,
    #[serde(default)]
    pub plan: MacroBounds,
    /// Category whitelist/blacklist applied to the candidate pool. Not counted
    /// by [`Self::is_empty`].
    #[serde(default)]
    pub category: CategoryFilter,
}

impl NutritionBounds {
    /// True when no macro constraints are set. The [`CategoryFilter`] is not
    /// considered.
    pub fn is_empty(&self) -> bool {
        self.per_day.is_empty() && self.per_meal.is_empty() && self.plan.is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        self.per_day.validate("per_day")?;
        self.per_meal.validate("per_meal")?;
        self.plan.validate("plan")?;
        self.category.validate()?;
        Ok(())
    }

    pub fn from_toml_str(text: &str) -> Result<Self> {
        let bounds: Self = toml::from_str(text).context("parsing nutrition bounds TOML")?;
        bounds.validate()?;
        Ok(bounds)
    }

    pub fn from_toml_path(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading nutrition config {}", path.display()))?;
        Self::from_toml_str(&text)
            .with_context(|| format!("invalid nutrition config {}", path.display()))
    }

    /// Serialize to TOML for GUI save / round-trip.
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serializing nutrition bounds to TOML")
    }

    /// Overlay CLI per-day flags. Only `Some` fields replace file values.
    pub fn merge_cli_per_day(&mut self, cli: &CliPerDayNutrition) {
        merge_range(&mut self.per_day.kcal, cli.min_kcal, cli.max_kcal);
        merge_range(
            &mut self.per_day.protein_g,
            cli.min_protein_g,
            cli.max_protein_g,
        );
        merge_range(&mut self.per_day.fat_g, cli.min_fat_g, cli.max_fat_g);
        merge_range(&mut self.per_day.carbs_g, cli.min_carbs_g, cli.max_carbs_g);
    }
}

fn merge_range(range: &mut MacroRange, min: Option<f64>, max: Option<f64>) {
    if min.is_some() {
        range.min = min;
    }
    if max.is_some() {
        range.max = max;
    }
}

/// Per-day CLI overlays (all optional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliPerDayNutrition {
    pub min_kcal: Option<f64>,
    pub max_kcal: Option<f64>,
    pub min_protein_g: Option<f64>,
    pub max_protein_g: Option<f64>,
    pub min_fat_g: Option<f64>,
    pub max_fat_g: Option<f64>,
    pub min_carbs_g: Option<f64>,
    pub max_carbs_g: Option<f64>,
}

impl CliPerDayNutrition {
    pub fn is_empty(&self) -> bool {
        self.min_kcal.is_none()
            && self.max_kcal.is_none()
            && self.min_protein_g.is_none()
            && self.max_protein_g.is_none()
            && self.min_fat_g.is_none()
            && self.max_fat_g.is_none()
            && self.min_carbs_g.is_none()
            && self.max_carbs_g.is_none()
    }
}

/// Build bounds from optional config path + CLI overlays.
pub fn load_nutrition_bounds(
    config_path: Option<&std::path::Path>,
    cli: &CliPerDayNutrition,
) -> Result<NutritionBounds> {
    let mut bounds = match config_path {
        Some(p) => NutritionBounds::from_toml_path(p)?,
        None => NutritionBounds::default(),
    };
    bounds.merge_cli_per_day(cli);
    bounds.validate()?;
    Ok(bounds)
}

/// Which macro a violation refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NutrientKind {
    Kcal,
    ProteinG,
    FatG,
    CarbsG,
}

impl NutrientKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kcal => "kcal",
            Self::ProteinG => "protein_g",
            Self::FatG => "fat_g",
            Self::CarbsG => "carbs_g",
        }
    }
}

impl fmt::Display for NutrientKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    BelowMin,
    AboveMax,
    RatioBelowTarget,
    RatioAboveTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoundScope {
    PerDay { day: u32 },
    PerMeal { day: u32, meal: u32 },
    Plan,
}

impl fmt::Display for BoundScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PerDay { day } => write!(f, "day {}", day + 1),
            Self::PerMeal { day, meal } => write!(f, "day {} meal {}", day + 1, meal + 1),
            Self::Plan => write!(f, "plan total"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundViolation {
    pub scope: BoundScope,
    pub nutrient: NutrientKind,
    pub kind: ViolationKind,
    /// Actual value: grams/kcal for min/max, or the actual share (%) for ratio.
    pub actual: f64,
    /// Bound value: the min/max, or the target share (%) for ratio.
    pub bound: f64,
    /// How far outside the bound/band, in the metric's unit (grams or kcal).
    pub magnitude: f64,
}

impl fmt::Display for BoundViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ViolationKind::BelowMin => write!(
                f,
                "{} {} {:.1} below min {:.1}",
                self.scope, self.nutrient, self.actual, self.bound
            ),
            ViolationKind::AboveMax => write!(
                f,
                "{} {} {:.1} above max {:.1}",
                self.scope, self.nutrient, self.actual, self.bound
            ),
            ViolationKind::RatioBelowTarget => write!(
                f,
                "{} {} ratio {:.1}% below target {:.1}%",
                self.scope, self.nutrient, self.actual, self.bound
            ),
            ViolationKind::RatioAboveTarget => write!(
                f,
                "{} {} ratio {:.1}% above target {:.1}%",
                self.scope, self.nutrient, self.actual, self.bound
            ),
        }
    }
}

/// Evaluate one scope's bounds against totals.
pub fn evaluate_macros(
    bounds: &MacroBounds,
    totals: &Macros,
    scope: BoundScope,
) -> Vec<BoundViolation> {
    let mut out = Vec::new();
    check_range(
        &mut out,
        &scope,
        NutrientKind::Kcal,
        totals.kcal,
        &bounds.kcal,
    );
    check_range(
        &mut out,
        &scope,
        NutrientKind::ProteinG,
        totals.protein_g,
        &bounds.protein_g,
    );
    check_range(
        &mut out,
        &scope,
        NutrientKind::FatG,
        totals.fat_g,
        &bounds.fat_g,
    );
    check_range(
        &mut out,
        &scope,
        NutrientKind::CarbsG,
        totals.carbs_g,
        &bounds.carbs_g,
    );
    check_ratio(&mut out, &scope, totals, &bounds.ratio);
    out
}

fn check_range(
    out: &mut Vec<BoundViolation>,
    scope: &BoundScope,
    nutrient: NutrientKind,
    actual: f64,
    range: &MacroRange,
) {
    if let Some(min) = range.min {
        if actual < min {
            out.push(BoundViolation {
                scope: scope.clone(),
                nutrient,
                kind: ViolationKind::BelowMin,
                actual,
                bound: min,
                magnitude: (min - actual).max(0.0),
            });
        }
    }
    if let Some(max) = range.max {
        if actual > max {
            out.push(BoundViolation {
                scope: scope.clone(),
                nutrient,
                kind: ViolationKind::AboveMax,
                actual,
                bound: max,
                magnitude: (actual - max).max(0.0),
            });
        }
    }
}

/// Emit a violation for each specified macro share that falls outside its
/// tolerance band. Shares are of total macro grams; magnitude is grams beyond
/// the band. Skips scopes with no macro grams.
fn check_ratio(
    out: &mut Vec<BoundViolation>,
    scope: &BoundScope,
    totals: &Macros,
    ratio: &MacroRatio,
) {
    if ratio.is_empty() {
        return;
    }
    let base = totals.protein_g + totals.fat_g + totals.carbs_g;
    if base <= 0.0 {
        return;
    }
    let tol_g = ratio.effective_tolerance() / 100.0 * base;
    for (nutrient, actual_g, target) in [
        (NutrientKind::ProteinG, totals.protein_g, ratio.protein),
        (NutrientKind::FatG, totals.fat_g, ratio.fat),
        (NutrientKind::CarbsG, totals.carbs_g, ratio.carb),
    ] {
        let Some(target_pct) = target else {
            continue;
        };
        let target_g = target_pct / 100.0 * base;
        let actual_pct = actual_g / base * 100.0;
        let dev = actual_g - target_g;
        if dev > tol_g {
            out.push(BoundViolation {
                scope: scope.clone(),
                nutrient,
                kind: ViolationKind::RatioAboveTarget,
                actual: actual_pct,
                bound: target_pct,
                magnitude: dev - tol_g,
            });
        } else if dev < -tol_g {
            out.push(BoundViolation {
                scope: scope.clone(),
                nutrient,
                kind: ViolationKind::RatioBelowTarget,
                actual: actual_pct,
                bound: target_pct,
                magnitude: -dev - tol_g,
            });
        }
    }
}

pub fn violation_magnitude(violations: &[BoundViolation]) -> f64 {
    violations.iter().map(|v| v.magnitude).sum()
}

// --- Constraint weighting for plan ranking --------------------------------
//
// Ranking normalizes each violation to a comparable scale (÷ a per-kind
// reference) and weights calories and the macro-split ratio as the headline
// constraints. Raw magnitudes are unchanged for the reported violation list.

/// Reference size of a "meaningful" violation, per kind (normalizes units).
const KCAL_REF: f64 = 100.0; // kcal
const GRAM_REF: f64 = 15.0; // grams (protein/fat/carb min/max)
const RATIO_GRAM_REF: f64 = 15.0; // grams beyond the ratio band

/// Priority multipliers: calories and ratio are ~5× a comparable macro miss.
const KCAL_PRIORITY: f64 = 5.0;
const RATIO_PRIORITY: f64 = 5.0;
const MACRO_PRIORITY: f64 = 1.0;

/// Per-unit ranking weight for a calorie min/max violation.
pub const KCAL_WEIGHT: f64 = KCAL_PRIORITY / KCAL_REF;
/// Per-unit ranking weight for a protein/fat/carb min/max violation.
pub const MACRO_WEIGHT: f64 = MACRO_PRIORITY / GRAM_REF;
/// Per-unit ranking weight for a macro-split ratio violation.
pub const RATIO_WEIGHT: f64 = RATIO_PRIORITY / RATIO_GRAM_REF;

fn weighted_one(v: &BoundViolation) -> f64 {
    let w = match v.kind {
        ViolationKind::RatioBelowTarget | ViolationKind::RatioAboveTarget => RATIO_WEIGHT,
        ViolationKind::BelowMin | ViolationKind::AboveMax => match v.nutrient {
            NutrientKind::Kcal => KCAL_WEIGHT,
            _ => MACRO_WEIGHT,
        },
    };
    w * v.magnitude
}

/// Weighted, unit-normalized total used to *rank* infeasible plans (calories +
/// ratio prioritized). Zero exactly when there are no violations.
pub fn weighted_magnitude(violations: &[BoundViolation]) -> f64 {
    violations.iter().map(weighted_one).sum()
}

/// How far `totals` are below configured mins (0 if met or unset).
pub fn min_deficit(bounds: &MacroBounds, totals: &Macros) -> f64 {
    let mut d = 0.0;
    if let Some(min) = bounds.kcal.min {
        d += (min - totals.kcal).max(0.0);
    }
    if let Some(min) = bounds.protein_g.min {
        d += (min - totals.protein_g).max(0.0);
    }
    if let Some(min) = bounds.fat_g.min {
        d += (min - totals.fat_g).max(0.0);
    }
    if let Some(min) = bounds.carbs_g.min {
        d += (min - totals.carbs_g).max(0.0);
    }
    d
}

/// True if adding `add` to `current` would exceed any configured max.
pub fn exceeds_max(bounds: &MacroBounds, current: &Macros, add: &Macros) -> bool {
    exceeds_one(bounds.kcal.max, current.kcal, add.kcal)
        || exceeds_one(bounds.protein_g.max, current.protein_g, add.protein_g)
        || exceeds_one(bounds.fat_g.max, current.fat_g, add.fat_g)
        || exceeds_one(bounds.carbs_g.max, current.carbs_g, add.carbs_g)
}

fn exceeds_one(max: Option<f64>, current: f64, add: f64) -> bool {
    match max {
        Some(m) => current + add > m,
        None => false,
    }
}

/// True if `macros` alone violates any per-meal bound.
pub fn violates_per_meal(bounds: &MacroBounds, macros: &Macros) -> bool {
    !evaluate_macros(bounds, macros, BoundScope::PerMeal { day: 0, meal: 0 }).is_empty()
}

/// Evaluate all scopes for a completed schedule.
pub fn evaluate_schedule(
    bounds: &NutritionBounds,
    per_day: &[(u32, Macros)],
    per_meal: &[(u32, u32, Macros)],
    plan_total: &Macros,
) -> Vec<BoundViolation> {
    let mut out = Vec::new();
    if !bounds.per_day.is_empty() {
        for &(day, ref m) in per_day {
            out.extend(evaluate_macros(
                &bounds.per_day,
                m,
                BoundScope::PerDay { day },
            ));
        }
    }
    if !bounds.per_meal.is_empty() {
        for &(day, meal, ref m) in per_meal {
            out.extend(evaluate_macros(
                &bounds.per_meal,
                m,
                BoundScope::PerMeal { day, meal },
            ));
        }
    }
    if !bounds.plan.is_empty() {
        out.extend(evaluate_macros(&bounds.plan, plan_total, BoundScope::Plan));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bounds_are_empty() {
        assert!(NutritionBounds::default().is_empty());
        assert!(MacroBounds::default().is_empty());
    }

    #[test]
    fn validate_rejects_min_greater_than_max() {
        let err = NutritionBounds::from_toml_str(
            r#"
            [per_day]
            protein_g = { min = 100.0, max = 50.0 }
            "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("min") && msg.contains("max"), "{msg}");
    }

    #[test]
    fn toml_round_trip_all_scopes() {
        let text = r#"
            [per_day]
            protein_g = { min = 50.0, max = 200.0 }
            kcal = { max = 3000.0 }

            [per_meal]
            protein_g = { min = 15.0 }

            [plan]
            protein_g = { min = 350.0 }
        "#;
        let b = NutritionBounds::from_toml_str(text).unwrap();
        assert_eq!(b.per_day.protein_g.min, Some(50.0));
        assert_eq!(b.per_day.protein_g.max, Some(200.0));
        assert_eq!(b.per_day.kcal.max, Some(3000.0));
        assert_eq!(b.per_meal.protein_g.min, Some(15.0));
        assert_eq!(b.plan.protein_g.min, Some(350.0));
        assert!(!b.is_empty());
    }

    #[test]
    fn category_filter_default_allows_everything() {
        let f = CategoryFilter::default();
        assert!(f.is_empty());
        assert!(f.allows(Some("Sauce")));
        assert!(f.allows(None));
    }

    #[test]
    fn category_blacklist_excludes_matching_case_insensitive() {
        let f = CategoryFilter {
            blacklist: vec!["Sauce".into(), "Dressing".into()],
            ..Default::default()
        };
        assert!(!f.is_empty());
        assert!(!f.allows(Some("sauce"))); // case-insensitive
        assert!(!f.allows(Some("Main Course, Sauce"))); // any token matches
        assert!(f.allows(Some("Main Course")));
        assert!(!f.allows(None)); // any configured list excludes uncategorized
        assert!(!f.allows(Some("")));
        assert!(!f.allows(Some("  ,  ")));
        assert!(f.allows(Some("Applesauce Cake"))); // token equality, not substring
    }

    #[test]
    fn category_blacklist_alone_excludes_uncategorized() {
        let f = CategoryFilter {
            blacklist: vec!["Sauce".into()],
            ..Default::default()
        };
        assert!(!f.allows(None));
        assert!(!f.allows(Some("")));
        assert!(f.allows(Some("Dinner")));
    }

    #[test]
    fn category_whitelist_is_strict() {
        let f = CategoryFilter {
            whitelist: vec!["Main Course".into(), "Dinner".into()],
            ..Default::default()
        };
        assert!(f.allows(Some("main course"))); // matches (case-insensitive)
        assert!(f.allows(Some("Sauce, Dinner"))); // any token matches whitelist
        assert!(!f.allows(Some("Dessert"))); // not on whitelist
        assert!(!f.allows(None)); // strict: uncategorized excluded
    }

    #[test]
    fn category_blacklist_beats_whitelist() {
        let f = CategoryFilter {
            whitelist: vec!["Main Course".into()],
            blacklist: vec!["Sauce".into()],
        };
        assert!(!f.allows(Some("Main Course, Sauce"))); // both tokens -> excluded
        assert!(f.allows(Some("Main Course")));
    }

    #[test]
    fn category_config_entry_may_bundle_comma_values() {
        // A single comma-joined string counts as multiple tokens.
        let f = CategoryFilter {
            blacklist: vec!["Sauce, Dip".into()],
            ..Default::default()
        };
        assert!(!f.allows(Some("Sauce")));
        assert!(!f.allows(Some("Dip")));
        assert!(f.allows(Some("Main Course")));
        // Overlap is caught at the token level.
        let overlap = CategoryFilter {
            whitelist: vec!["Main Course, Sauce".into()],
            blacklist: vec!["sauce".into()],
        };
        assert!(overlap.validate().is_err());
    }

    #[test]
    fn category_validate_rejects_overlap_and_empty() {
        let overlap = CategoryFilter {
            whitelist: vec!["Sauce".into()],
            blacklist: vec!["sauce".into()],
        };
        assert!(overlap.validate().is_err());

        let empty_entry = CategoryFilter {
            blacklist: vec!["  ".into()],
            ..Default::default()
        };
        assert!(empty_entry.validate().is_err());
    }

    #[test]
    fn toml_parses_category_table() {
        let text = r#"
            [category]
            whitelist = ["Main Course", "Dinner"]
            blacklist = ["Sauce", "Dressing"]
        "#;
        let b = NutritionBounds::from_toml_str(text).unwrap();
        assert_eq!(b.category.whitelist, vec!["Main Course", "Dinner"]);
        assert_eq!(b.category.blacklist, vec!["Sauce", "Dressing"]);
        // A category-only config must not read as non-empty macro bounds.
        assert!(
            b.is_empty(),
            "category-only config must keep is_empty() true"
        );
        assert!(!b.category.is_empty());
    }

    #[test]
    fn toml_rejects_category_overlap() {
        let err = NutritionBounds::from_toml_str(
            r#"
            [category]
            whitelist = ["Main Course"]
            blacklist = ["main course"]
            "#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("category"), "{err:#}");
    }

    #[test]
    fn cli_overlay_replaces_per_day_fields_only() {
        let mut b = NutritionBounds::from_toml_str(
            r#"
            [per_day]
            protein_g = { min = 40.0, max = 100.0 }
            [per_meal]
            protein_g = { min = 10.0 }
            "#,
        )
        .unwrap();
        b.merge_cli_per_day(&CliPerDayNutrition {
            min_protein_g: Some(50.0),
            max_kcal: Some(2500.0),
            ..Default::default()
        });
        assert_eq!(b.per_day.protein_g.min, Some(50.0));
        assert_eq!(b.per_day.protein_g.max, Some(100.0)); // untouched
        assert_eq!(b.per_day.kcal.max, Some(2500.0));
        assert_eq!(b.per_meal.protein_g.min, Some(10.0)); // untouched
        b.validate().unwrap();
    }

    #[test]
    fn load_nutrition_bounds_cli_only() {
        let b = load_nutrition_bounds(
            None,
            &CliPerDayNutrition {
                min_protein_g: Some(60.0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(b.per_day.protein_g.min, Some(60.0));
        assert!(b.per_meal.is_empty());
    }

    #[test]
    fn load_rejects_cli_min_gt_max() {
        let err = load_nutrition_bounds(
            None,
            &CliPerDayNutrition {
                min_protein_g: Some(100.0),
                max_protein_g: Some(10.0),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("protein_g"));
    }

    #[test]
    fn evaluate_macros_below_min_and_above_max() {
        let bounds = MacroBounds {
            protein_g: MacroRange {
                min: Some(50.0),
                max: Some(100.0),
            },
            ..Default::default()
        };
        let low = Macros {
            protein_g: 20.0,
            ..Default::default()
        };
        let v = evaluate_macros(&bounds, &low, BoundScope::PerDay { day: 0 });
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::BelowMin);
        assert!((v[0].magnitude - 30.0).abs() < 1e-9);

        let high = Macros {
            protein_g: 150.0,
            ..Default::default()
        };
        let v = evaluate_macros(&bounds, &high, BoundScope::PerDay { day: 1 });
        assert_eq!(v[0].kind, ViolationKind::AboveMax);
        assert!((v[0].magnitude - 50.0).abs() < 1e-9);
    }

    #[test]
    fn empty_bounds_produce_no_violations() {
        let v = evaluate_macros(
            &MacroBounds::default(),
            &Macros {
                kcal: 9999.0,
                protein_g: 0.0,
                fat_g: 0.0,
                carbs_g: 0.0,
            },
            BoundScope::Plan,
        );
        assert!(v.is_empty());
        assert_eq!(violation_magnitude(&v), 0.0);
    }

    #[test]
    fn violation_magnitude_sums() {
        let vs = vec![
            BoundViolation {
                scope: BoundScope::Plan,
                nutrient: NutrientKind::ProteinG,
                kind: ViolationKind::BelowMin,
                actual: 10.0,
                bound: 40.0,
                magnitude: 30.0,
            },
            BoundViolation {
                scope: BoundScope::Plan,
                nutrient: NutrientKind::Kcal,
                kind: ViolationKind::AboveMax,
                actual: 120.0,
                bound: 100.0,
                magnitude: 20.0,
            },
        ];
        assert!((violation_magnitude(&vs) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn min_deficit_and_exceeds_max_helpers() {
        let bounds = MacroBounds {
            protein_g: MacroRange {
                min: Some(50.0),
                max: Some(100.0),
            },
            ..Default::default()
        };
        let cur = Macros {
            protein_g: 20.0,
            ..Default::default()
        };
        assert!((min_deficit(&bounds, &cur) - 30.0).abs() < 1e-9);
        let add = Macros {
            protein_g: 90.0,
            ..Default::default()
        };
        assert!(exceeds_max(&bounds, &cur, &add));
        let small = Macros {
            protein_g: 10.0,
            ..Default::default()
        };
        assert!(!exceeds_max(&bounds, &cur, &small));
    }

    #[test]
    fn evaluate_schedule_covers_all_scopes() {
        let bounds = NutritionBounds::from_toml_str(
            r#"
            [per_day]
            protein_g = { min = 50.0 }
            [per_meal]
            protein_g = { min = 20.0 }
            [plan]
            protein_g = { min = 100.0 }
            "#,
        )
        .unwrap();
        let per_day = vec![(
            0,
            Macros {
                protein_g: 40.0,
                ..Default::default()
            },
        )];
        let per_meal = vec![(
            0,
            0,
            Macros {
                protein_g: 10.0,
                ..Default::default()
            },
        )];
        let plan = Macros {
            protein_g: 40.0,
            ..Default::default()
        };
        let v = evaluate_schedule(&bounds, &per_day, &per_meal, &plan);
        assert_eq!(v.len(), 3);
        assert!((violation_magnitude(&v) - (10.0 + 10.0 + 60.0)).abs() < 1e-9);
    }

    #[test]
    fn ratio_toml_round_trip() {
        let b = NutritionBounds::from_toml_str(
            r#"
            [per_day]
            ratio = { protein = 35, fat = 30, carb = 35 }
            [per_meal]
            ratio = { protein = 40, tolerance = 8 }
            "#,
        )
        .unwrap();
        assert_eq!(b.per_day.ratio.protein, Some(35.0));
        assert_eq!(b.per_day.ratio.carb, Some(35.0));
        assert_eq!(b.per_day.ratio.tolerance, None);
        assert!((b.per_day.ratio.effective_tolerance() - DEFAULT_RATIO_TOLERANCE).abs() < 1e-9);
        assert_eq!(b.per_meal.ratio.tolerance, Some(8.0));
        assert!(!b.is_empty());
    }

    #[test]
    fn ratio_validate_rejects_out_of_range_share() {
        let err = NutritionBounds::from_toml_str(
            r#"
            [per_day]
            ratio = { protein = 150 }
            "#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("ratio"), "{err:#}");
    }

    #[test]
    fn ratio_within_band_produces_no_violation() {
        let bounds = MacroBounds {
            ratio: MacroRatio {
                protein: Some(35.0),
                fat: Some(30.0),
                carb: Some(35.0),
                tolerance: None, // default 5 pts
            },
            ..Default::default()
        };
        // 37/30/33 of 100 g total — all within +/-5 of target.
        let totals = Macros {
            protein_g: 37.0,
            fat_g: 30.0,
            carbs_g: 33.0,
            ..Default::default()
        };
        let v = evaluate_macros(&bounds, &totals, BoundScope::PerDay { day: 0 });
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn ratio_out_of_band_reports_grams_beyond_band() {
        let bounds = MacroBounds {
            ratio: MacroRatio {
                protein: Some(35.0),
                fat: Some(30.0),
                carb: Some(35.0),
                tolerance: None, // default 5 pts => 5 g of 100
            },
            ..Default::default()
        };
        // 50/25/25 of 100 g: protein 50% (15 over target, 10 beyond band),
        // carb 25% (10 under target, 5 beyond band), fat 25% (5 under => within band).
        let totals = Macros {
            protein_g: 50.0,
            fat_g: 25.0,
            carbs_g: 25.0,
            ..Default::default()
        };
        let v = evaluate_macros(&bounds, &totals, BoundScope::PerDay { day: 0 });
        assert_eq!(v.len(), 2, "{v:?}");
        let protein = v
            .iter()
            .find(|x| x.nutrient == NutrientKind::ProteinG)
            .unwrap();
        assert_eq!(protein.kind, ViolationKind::RatioAboveTarget);
        assert!((protein.actual - 50.0).abs() < 1e-9);
        assert!((protein.bound - 35.0).abs() < 1e-9);
        assert!((protein.magnitude - 10.0).abs() < 1e-9);
        let carb = v
            .iter()
            .find(|x| x.nutrient == NutrientKind::CarbsG)
            .unwrap();
        assert_eq!(carb.kind, ViolationKind::RatioBelowTarget);
        assert!((carb.magnitude - 5.0).abs() < 1e-9);
        assert!((violation_magnitude(&v) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn ratio_skipped_when_no_macro_grams() {
        let bounds = MacroBounds {
            ratio: MacroRatio {
                protein: Some(35.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = evaluate_macros(&bounds, &Macros::default(), BoundScope::Plan);
        assert!(v.is_empty());
    }

    #[test]
    fn ratio_tolerance_override_tightens_band() {
        let bounds = MacroBounds {
            ratio: MacroRatio {
                protein: Some(35.0),
                tolerance: Some(1.0), // +/-1 pt
                ..Default::default()
            },
            ..Default::default()
        };
        // 37% protein: within default 5 but outside a 1-pt band (1 g beyond).
        let totals = Macros {
            protein_g: 37.0,
            fat_g: 30.0,
            carbs_g: 33.0,
            ..Default::default()
        };
        let v = evaluate_macros(&bounds, &totals, BoundScope::PerDay { day: 0 });
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::RatioAboveTarget);
        assert!((v[0].magnitude - 1.0).abs() < 1e-9);
    }

    #[test]
    fn weighting_prioritizes_calories_and_ratio() {
        let viol = |nutrient, kind, magnitude| BoundViolation {
            scope: BoundScope::Plan,
            nutrient,
            kind,
            actual: 0.0,
            bound: 0.0,
            magnitude,
        };
        // A 100 kcal miss, a 15 g protein-min miss, and a 15 g ratio miss.
        let kcal = weighted_magnitude(&[viol(NutrientKind::Kcal, ViolationKind::AboveMax, 100.0)]);
        let protein =
            weighted_magnitude(&[viol(NutrientKind::ProteinG, ViolationKind::BelowMin, 15.0)]);
        let ratio = weighted_magnitude(&[viol(
            NutrientKind::FatG,
            ViolationKind::RatioAboveTarget,
            15.0,
        )]);
        assert!((protein - 1.0).abs() < 1e-9, "macro baseline: {protein}");
        assert!((kcal - 5.0).abs() < 1e-9, "calories ~5x: {kcal}");
        assert!((ratio - 5.0).abs() < 1e-9, "ratio ~5x: {ratio}");
        assert!(kcal > protein && ratio > protein);
        // No violations scores 0.
        assert_eq!(weighted_magnitude(&[]), 0.0);
    }
}
