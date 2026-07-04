//! Manual / file-based recipe ingestion (JSON or TOML).

use super::RecipeSourceIngest;
use crate::domain::{Recipe, RecipeMeta, RecipeSource};
use crate::normalize::normalize_line;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Intermediate format accepted from JSON/TOML files.
#[derive(Debug, Deserialize)]
struct RecipeFile {
    title: String,
    #[serde(default)]
    servings: Option<f64>,
    /// Either structured ingredients or free-text lines.
    #[serde(default)]
    ingredients: Vec<IngredientInput>,
    #[serde(default)]
    steps: Vec<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    cuisine: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    prep_time_minutes: Option<u32>,
    #[serde(default)]
    cook_time_minutes: Option<u32>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum IngredientInput {
    Text(String),
    Structured {
        #[serde(default)]
        original: Option<String>,
        name: Option<String>,
        quantity: Option<f64>,
        unit: Option<String>,
        note: Option<String>,
    },
}

pub struct FileSource;

impl RecipeSourceIngest for FileSource {
    fn name(&self) -> &'static str {
        "file"
    }

    fn ingest(&self, input: &str) -> Result<Recipe> {
        let path = Path::new(input);
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading recipe file {}", path.display()))?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let file: RecipeFile = match ext.as_str() {
            "toml" => toml::from_str(&text).context("parsing TOML recipe")?,
            "json" => serde_json::from_str(&text).context("parsing JSON recipe")?,
            "txt" | "md" => return Ok(parse_plain_text(&text, path)),
            _ => {
                // Try JSON then TOML
                if let Ok(r) = serde_json::from_str::<RecipeFile>(&text) {
                    r
                } else if let Ok(r) = toml::from_str::<RecipeFile>(&text) {
                    r
                } else {
                    bail!("unsupported or invalid recipe file: {}", path.display());
                }
            }
        };

        Ok(file_to_recipe(
            file,
            RecipeSource::File {
                path: path.display().to_string(),
            },
        ))
    }
}

fn file_to_recipe(file: RecipeFile, source: RecipeSource) -> Recipe {
    let mut recipe = Recipe::new(file.title);
    recipe.servings = file.servings;
    recipe.steps = file.steps;
    recipe.meta = RecipeMeta {
        author: file.author,
        cuisine: file.cuisine,
        category: file.category,
        tags: file.tags,
        prep_time_minutes: file.prep_time_minutes,
        cook_time_minutes: file.cook_time_minutes,
        source_url: file.source_url,
        notes: file.notes,
        nutrition: None,
    };
    recipe.source = source;
    recipe.ingredients = file
        .ingredients
        .into_iter()
        .map(|ing| match ing {
            IngredientInput::Text(s) => normalize_line(&s),
            IngredientInput::Structured {
                original,
                name,
                quantity,
                unit,
                note,
            } => {
                let original = original
                    .or_else(|| {
                        let mut parts = Vec::new();
                        if let Some(q) = quantity {
                            parts.push(format_qty(q));
                        }
                        if let Some(ref u) = unit {
                            parts.push(u.clone());
                        }
                        if let Some(ref n) = name {
                            parts.push(n.clone());
                        }
                        if parts.is_empty() {
                            None
                        } else {
                            Some(parts.join(" "))
                        }
                    })
                    .unwrap_or_default();
                if let Some(n) = name {
                    // Prefer structured fields when provided fully
                    use crate::normalize::resolve_unit;
                    let unit = unit.map(|u| resolve_unit(&u));
                    crate::domain::IngredientLine {
                        original: if original.is_empty() {
                            n.clone()
                        } else {
                            original
                        },
                        name: n,
                        quantity,
                        unit,
                        note,
                        parse_uncertain: quantity.is_none(),
                    }
                } else {
                    normalize_line(&original)
                }
            }
        })
        .collect();
    recipe
}

fn format_qty(q: f64) -> String {
    if (q - q.round()).abs() < 1e-9 {
        format!("{}", q.round() as i64)
    } else {
        format!("{q}")
    }
}

/// Very simple plain-text format:
/// First non-empty line = title
/// Lines after `Ingredients:` until `Steps:` / `Instructions:` = ingredient lines
/// Rest = steps
fn parse_plain_text(text: &str, path: &Path) -> Recipe {
    let mut title = String::from("Untitled");
    let mut ingredients = Vec::new();
    let mut steps = Vec::new();
    let mut section = "title";

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_lowercase();
        if lower.starts_with("ingredients") && t.contains(':') {
            section = "ingredients";
            continue;
        }
        if lower.starts_with("steps")
            || lower.starts_with("instructions")
            || lower.starts_with("directions")
        {
            section = "steps";
            continue;
        }
        match section {
            "title" => {
                title = t.to_string();
                section = "body";
            }
            "ingredients" => {
                ingredients.push(normalize_line(t.trim_start_matches(['-', '*', '•'])))
            }
            "steps" => steps.push(
                t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
                    .trim()
                    .to_string(),
            ),
            _ => {
                // Ambiguous body: treat as ingredient lines until we see steps header
                ingredients.push(normalize_line(t.trim_start_matches(['-', '*', '•'])));
            }
        }
    }

    let mut recipe = Recipe::new(title);
    recipe.ingredients = ingredients;
    recipe.steps = steps;
    recipe.source = RecipeSource::File {
        path: path.display().to_string(),
    };
    recipe
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn ingest_json() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"{{"title":"Toast","servings":1,"ingredients":["2 slices bread","1 tbsp butter"],"steps":["Toast bread","Butter it"]}}"#
        )
        .unwrap();
        let r = FileSource.ingest(f.path().to_str().unwrap()).unwrap();
        assert_eq!(r.title, "Toast");
        assert_eq!(r.ingredients.len(), 2);
        assert_eq!(r.steps.len(), 2);
    }

    #[test]
    fn ingest_toml() {
        let mut f = NamedTempFile::with_suffix(".toml").unwrap();
        write!(
            f,
            r#"
title = "Soup"
servings = 4.0
ingredients = ["2 cups broth", "1 onion"]
steps = ["Simmer"]
"#
        )
        .unwrap();
        let r = FileSource.ingest(f.path().to_str().unwrap()).unwrap();
        assert_eq!(r.title, "Soup");
        assert_eq!(r.ingredients.len(), 2);
    }
}
