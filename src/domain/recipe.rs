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
    pub tags: Vec<String>,
    pub prep_time_minutes: Option<u32>,
    pub cook_time_minutes: Option<u32>,
    pub source_url: Option<String>,
    /// Free-form notes.
    pub notes: Option<String>,
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
