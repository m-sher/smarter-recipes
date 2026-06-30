//! Image / OCR recipe ingestion.
//!
//! Uses external `tesseract` when available. If OCR is unavailable, accepts a
//! companion `.txt` sidecar with the same stem, or returns a clear error with
//! install instructions. This keeps the core testable without Tesseract.

use super::RecipeSourceIngest;
use crate::domain::{Recipe, RecipeSource};
use crate::normalize::normalize_line;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

pub struct ImageOcrSource;

impl RecipeSourceIngest for ImageOcrSource {
    fn name(&self) -> &'static str {
        "image"
    }

    fn ingest(&self, input: &str) -> Result<Recipe> {
        let path = Path::new(input);
        if !path.exists() {
            bail!("image not found: {}", path.display());
        }

        // Prefer sidecar text for reproducibility / offline use
        let sidecar = path.with_extension("txt");
        let text = if sidecar.exists() {
            std::fs::read_to_string(&sidecar)
                .with_context(|| format!("reading sidecar {}", sidecar.display()))?
        } else {
            run_tesseract(path)?
        };

        let recipe = text_to_recipe(&text, path);
        Ok(recipe)
    }
}

fn run_tesseract(path: &Path) -> Result<String> {
    let output = Command::new("tesseract")
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg("eng")
        .output();

    match output {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("tesseract failed: {stderr}");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "tesseract not found on PATH. Install Tesseract OCR, or place a \
                 plain-text sidecar next to the image (same name with .txt extension) \
                 containing the recipe text."
            );
        }
        Err(e) => Err(e).context("running tesseract")?,
    }
}

fn text_to_recipe(text: &str, path: &Path) -> Recipe {
    let mut title = String::from("Scanned recipe");
    let mut ingredients = Vec::new();
    let mut steps = Vec::new();
    let mut section = "title";

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_lowercase();
        if lower.contains("ingredient") {
            section = "ingredients";
            continue;
        }
        if lower.contains("instruction")
            || lower.contains("direction")
            || lower.starts_with("steps")
            || lower.starts_with("method")
        {
            section = "steps";
            continue;
        }
        match section {
            "title" => {
                title = t.to_string();
                section = "ingredients"; // OCR dumps often list ingredients next
            }
            "ingredients" => {
                let cleaned = t.trim_start_matches(['-', '*', '•', '·']);
                ingredients.push(normalize_line(cleaned));
            }
            "steps" => {
                let cleaned =
                    t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')');
                steps.push(cleaned.trim().to_string());
            }
            _ => {}
        }
    }

    // If we never found structured sections, treat all non-title lines as ingredients.
    if ingredients.is_empty() && steps.is_empty() {
        for line in text.lines().skip(1) {
            let t = line.trim();
            if !t.is_empty() {
                ingredients.push(normalize_line(t));
            }
        }
    }

    let mut recipe = Recipe::new(title);
    recipe.ingredients = ingredients;
    recipe.steps = steps;
    recipe.source = RecipeSource::Image {
        path: path.display().to_string(),
    };
    recipe
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn sidecar_txt() {
        let dir = TempDir::new().unwrap();
        let img = dir.path().join("recipe.png");
        // Minimal fake image file (not decoded — sidecar is used)
        std::fs::write(&img, b"not-an-image").unwrap();
        let mut txt = std::fs::File::create(dir.path().join("recipe.txt")).unwrap();
        writeln!(txt, "Pancakes").unwrap();
        writeln!(txt, "Ingredients:").unwrap();
        writeln!(txt, "2 cups flour").unwrap();
        writeln!(txt, "1 cup milk").unwrap();
        writeln!(txt, "Steps:").unwrap();
        writeln!(txt, "Mix and cook.").unwrap();

        let r = ImageOcrSource.ingest(img.to_str().unwrap()).unwrap();
        assert_eq!(r.title, "Pancakes");
        assert_eq!(r.ingredients.len(), 2);
        assert_eq!(r.steps.len(), 1);
    }
}
