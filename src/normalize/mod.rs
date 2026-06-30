//! Free-text ingredient parsing and unit normalization.
//!
//! This module is the foundation for aggregation, planning, and shopping.
//! It intentionally has no I/O dependencies.

mod parse;
mod units;

pub use parse::{parse_ingredient_line, parse_ingredient_lines};
pub use units::{lookup_unit, register_alias, resolve_unit, CANONICAL_MASS, CANONICAL_VOLUME};

use crate::domain::{IngredientLine, ParsedIngredient};

/// Parse a free-text line into a full [`IngredientLine`], preserving the original text.
pub fn normalize_line(original: &str) -> IngredientLine {
    let parsed = parse_ingredient_line(original);
    IngredientLine {
        original: original.to_string(),
        name: parsed.name,
        quantity: parsed.quantity,
        unit: parsed.unit,
        note: parsed.note,
        parse_uncertain: parsed.uncertain,
    }
}

/// Convert a [`ParsedIngredient`] plus original text into an [`IngredientLine`].
pub fn from_parsed(original: &str, parsed: ParsedIngredient) -> IngredientLine {
    IngredientLine {
        original: original.to_string(),
        name: parsed.name,
        quantity: parsed.quantity,
        unit: parsed.unit,
        note: parsed.note,
        parse_uncertain: parsed.uncertain,
    }
}
