//! Interactive manual recipe entry.

use crate::domain::{Recipe, RecipeSource};
use crate::normalize::normalize_line;
use anyhow::{bail, Result};
use std::io::{BufRead, Write};

fn read_line<R: BufRead>(input: &mut R) -> Result<Option<String>> {
    let mut buf = String::new();
    if input.read_line(&mut buf)? == 0 {
        return Ok(None);
    }
    Ok(Some(buf.trim_end_matches(['\n', '\r']).to_string()))
}

/// Build a recipe from an interactive session: title, optional servings, then
/// ingredient lines and step lines, each terminated by a blank line or EOF.
/// Prompts are written to `prompts`; entered lines are read from `input`.
pub fn read_manual_recipe<R: BufRead, W: Write>(input: &mut R, prompts: &mut W) -> Result<Recipe> {
    write!(prompts, "Title: ")?;
    prompts.flush().ok();
    let title = read_line(input)?.unwrap_or_default().trim().to_string();
    if title.is_empty() {
        bail!("title is required");
    }

    write!(prompts, "Servings (optional): ")?;
    prompts.flush().ok();
    let servings = read_line(input)?.and_then(|s| s.trim().parse::<f64>().ok());

    writeln!(prompts, "Ingredients (one per line, blank to finish):")?;
    prompts.flush().ok();
    let mut ingredients = Vec::new();
    while let Some(line) = read_line(input)? {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        ingredients.push(normalize_line(line));
    }

    writeln!(prompts, "Steps (one per line, blank to finish):")?;
    prompts.flush().ok();
    let mut steps = Vec::new();
    while let Some(line) = read_line(input)? {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        steps.push(line.to_string());
    }

    let mut recipe = Recipe::new(title);
    recipe.servings = servings;
    recipe.ingredients = ingredients;
    recipe.steps = steps;
    recipe.source = RecipeSource::Manual;
    Ok(recipe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_full_recipe() {
        let script = "Omelette\n2\n2 large eggs\n1 tbsp butter\n\nBeat eggs\nCook in butter\n\n";
        let mut input = Cursor::new(script);
        let mut prompts = Vec::new();
        let r = read_manual_recipe(&mut input, &mut prompts).unwrap();
        assert_eq!(r.title, "Omelette");
        assert_eq!(r.servings, Some(2.0));
        assert_eq!(r.ingredients.len(), 2);
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.source, RecipeSource::Manual);
    }

    #[test]
    fn stops_at_eof_without_trailing_blanks() {
        let script = "Toast\n\n1 slice bread";
        let mut input = Cursor::new(script);
        let mut prompts = Vec::new();
        let r = read_manual_recipe(&mut input, &mut prompts).unwrap();
        assert_eq!(r.title, "Toast");
        assert_eq!(r.servings, None);
        assert_eq!(r.ingredients.len(), 1);
        assert!(r.steps.is_empty());
    }

    #[test]
    fn empty_title_errors() {
        let mut input = Cursor::new("\n");
        let mut prompts = Vec::new();
        assert!(read_manual_recipe(&mut input, &mut prompts).is_err());
    }
}
