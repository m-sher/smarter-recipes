//! Closed category vocabulary for LLM labeling and post-filter validation.
//!
//! Display casing is Title Case (or multi-word forms matching schema.org usage).
//! Matching uses [`crate::domain::normalize_title_key`]. Keep non-meal tokens
//! aligned with `examples/nutrition_bounds.toml` blacklist entries.

use crate::domain::normalize_title_key;
use std::collections::HashMap;

/// Canonical display labels the model may emit (and we store).
pub const ALLOWED_LABELS: &[&str] = &[
    // Meal / plan-eligible
    "Dinner",
    "Lunch",
    "Breakfast",
    "Brunch",
    "Main Course",
    "Main Dish",
    "Entree",
    "Side Dish",
    "Soup",
    "Stew",
    "Salad",
    "Appetizer",
    "Dessert",
    "Snack",
    "Sandwich",
    "Pasta",
    "Casserole",
    "Bread",
    "Breakfast and Brunch",
    // Non-meal / component (blacklist targets)
    "Beverage",
    "Drink",
    "Sauce",
    "Sauces",
    "Dressing",
    "Salad Dressing",
    "Condiment",
    "Condiments",
    "Dip",
    "Salsa",
    "Component",
    "Cooking Component",
    "Ingredient",
    "Spice Mix",
    "Seasonings",
    "Spices",
    "Staple",
    "Pantry",
    "Cooking Basics",
    "How-to",
];

/// Max category tokens we keep after filtering.
pub const MAX_LABELS: usize = 3;

/// Map normalized key → canonical display form.
fn display_by_key() -> HashMap<String, &'static str> {
    ALLOWED_LABELS
        .iter()
        .map(|&label| (normalize_title_key(label), label))
        .collect()
}

/// Filter raw model tokens to allowlisted display labels (deduped, max [`MAX_LABELS`]).
/// Unknown tokens are dropped. Empty input → empty vec.
pub fn filter_labels(raw: &[String]) -> Vec<String> {
    let map = display_by_key();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for token in raw {
        for part in token.split(',') {
            let key = normalize_title_key(part);
            if key.is_empty() {
                continue;
            }
            let Some(&display) = map.get(&key) else {
                continue;
            };
            if seen.insert(display) {
                out.push(display.to_string());
            }
            if out.len() >= MAX_LABELS {
                return out;
            }
        }
    }
    out
}

/// Join labels for storage (matches URL ingest: `", "`).
pub fn join_labels(labels: &[String]) -> Option<String> {
    if labels.is_empty() {
        None
    } else {
        Some(labels.join(", "))
    }
}

/// Comma-separated allowlist for prompts.
pub fn allowlist_for_prompt() -> String {
    ALLOWED_LABELS.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_keeps_allowlisted_case_insensitive() {
        let raw = vec!["beverage".into(), "DINNER".into(), "nonsense".into()];
        assert_eq!(
            filter_labels(&raw),
            vec!["Beverage".to_string(), "Dinner".to_string()]
        );
    }

    #[test]
    fn filter_splits_comma_bundles_and_dedupes() {
        let raw = vec!["Sauce, Condiment".into(), "sauce".into()];
        assert_eq!(
            filter_labels(&raw),
            vec!["Sauce".to_string(), "Condiment".to_string()]
        );
    }

    #[test]
    fn filter_caps_at_max() {
        let raw = vec![
            "Dinner".into(),
            "Lunch".into(),
            "Breakfast".into(),
            "Snack".into(),
        ];
        assert_eq!(filter_labels(&raw).len(), MAX_LABELS);
    }

    #[test]
    fn join_empty_is_none() {
        assert_eq!(join_labels(&[]), None);
        assert_eq!(join_labels(&["Dinner".into()]), Some("Dinner".to_string()));
    }
}
