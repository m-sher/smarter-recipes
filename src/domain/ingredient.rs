//! Ingredient lines and normalized ingredient identity.

use super::units::{Unit, UnitKind};
use serde::{Deserialize, Serialize};

/// A single ingredient as it appears on a recipe (before/after parsing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngredientLine {
    /// Original free-text line from the source.
    pub original: String,
    /// Parsed/normalized name (lowercased, trimmed of prep notes when possible).
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<Unit>,
    /// Optional preparation note (e.g. "diced", "room temperature").
    pub note: Option<String>,
    /// True when parsing was incomplete or ambiguous.
    pub parse_uncertain: bool,
}

impl IngredientLine {
    /// Quantity in the canonical base for this unit kind, if known.
    pub fn canonical_quantity(&self) -> Option<(f64, UnitKind)> {
        match (&self.quantity, &self.unit) {
            (Some(q), Some(u)) => Some((u.to_canonical(*q), u.kind)),
            (Some(q), None) => Some((*q, UnitKind::Count)),
            _ => None,
        }
    }
}

/// Result of parsing a free-text ingredient line.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<Unit>,
    pub note: Option<String>,
    pub uncertain: bool,
}

/// Stable key used to deduplicate ingredients across recipes.
///
/// Identity is `(normalized_name, unit_kind)` so we can aggregate quantities
/// of the same ingredient measured in compatible units.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IngredientKey {
    pub name: String,
    pub kind: UnitKind,
}

impl IngredientKey {
    pub fn from_line(line: &IngredientLine) -> Self {
        let kind = line
            .unit
            .as_ref()
            .map(|u| u.kind)
            .unwrap_or(UnitKind::Count);
        Self {
            name: normalize_ingredient_name(&line.name),
            kind,
        }
    }

    pub fn new(name: &str, kind: UnitKind) -> Self {
        Self {
            name: normalize_ingredient_name(name),
            kind,
        }
    }
}

/// Lowercase, collapse whitespace, strip trailing punctuation for identity.
pub fn normalize_ingredient_name(name: &str) -> String {
    let s = name.trim().to_lowercase();
    let s = s.trim_end_matches(['.', ',', ';']);
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}
