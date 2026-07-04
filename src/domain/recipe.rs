//! Recipe aggregate and metadata.

use super::ingredient::IngredientLine;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Opaque recipe identifier (UUID string in storage).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecipeId(pub String);

impl RecipeId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RecipeId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RecipeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RecipeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RecipeId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Where a recipe came from (for provenance / re-import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RecipeSource {
    File { path: String },
    Url { url: String },
    Image { path: String },
    Manual,
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecipeMeta {
    pub author: Option<String>,
    pub cuisine: Option<String>,
    /// schema.org `recipeCategory` — the publisher's own classification
    /// ("Main Course", "Sauce", "Dessert", …). Used to tell standalone meals
    /// from components (sauces, dressings, condiments). Joined when the source
    /// lists several.
    #[serde(default)]
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub prep_time_minutes: Option<u32>,
    pub cook_time_minutes: Option<u32>,
    pub source_url: Option<String>,
    /// Free-form notes.
    pub notes: Option<String>,
    /// Published per-serving nutrition (schema.org `NutritionInformation`).
    pub nutrition: Option<super::nutrition::Nutrition>,
}

/// Normalized recipe used throughout the system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub id: RecipeId,
    pub title: String,
    /// Yield / number of servings the ingredient quantities are for.
    pub servings: Option<f64>,
    pub ingredients: Vec<IngredientLine>,
    pub steps: Vec<String>,
    pub meta: RecipeMeta,
    pub source: RecipeSource,
}

impl Recipe {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            id: RecipeId::new(),
            title: title.into(),
            servings: None,
            ingredients: Vec::new(),
            steps: Vec::new(),
            meta: RecipeMeta::default(),
            source: RecipeSource::Manual,
        }
    }
}

/// Normalize a recipe title for identity / dedup comparison.
///
/// Trims, lowercases, maps curly/modifier apostrophes to `'`, and collapses
/// internal whitespace to single spaces.
pub fn normalize_title_key(title: &str) -> String {
    let mut s = title.trim().to_lowercase();
    for ch in ['\u{2018}', '\u{2019}', '\u{02BC}'] {
        s = s.replace(ch, "'");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::normalize_title_key;

    #[test]
    fn title_key_normalizes_case_space_apostrophe() {
        assert_eq!(
            normalize_title_key("  Grilled S'mores  "),
            normalize_title_key("grilled s'mores")
        );
        assert_eq!(
            normalize_title_key("Grilled S\u{2019}mores"), // right single quotation mark
            normalize_title_key("Grilled S'mores")
        );
        assert_eq!(normalize_title_key("A   B"), "a b");
    }
}
