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
/// Identity is `(normalized_name, unit_kind)`.
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

/// Lowercase, collapse whitespace, strip trailing punctuation and leading size
/// descriptors.
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
    strip_leading_descriptors(&out)
}

/// Remove leading size/quality descriptors. Leading-only; words that change the
/// ingredient itself (ground, whole, colors, brown/white, sweet, …) are excluded.
const MULTI_DESCRIPTORS: &[&str] = &["extra large", "extra-large"];
const SINGLE_DESCRIPTORS: &[&str] = &[
    "large", "medium", "small", "jumbo", "fresh", "ripe", "boneless", "skinless",
];

fn strip_leading_descriptors(name: &str) -> String {
    let mut s = name;
    loop {
        let mut stripped = None;
        for d in MULTI_DESCRIPTORS {
            if let Some(rest) = s.strip_prefix(d) {
                if let Some(rest) = rest.strip_prefix(' ') {
                    stripped = Some(rest);
                    break;
                }
            }
        }
        if stripped.is_none() {
            for d in SINGLE_DESCRIPTORS {
                if let Some(rest) = s.strip_prefix(d) {
                    if let Some(rest) = rest.strip_prefix(' ') {
                        stripped = Some(rest);
                        break;
                    }
                }
            }
        }
        match stripped {
            Some(rest) if !rest.trim().is_empty() => s = rest,
            _ => break,
        }
    }
    s.to_string()
}

/// Lookup candidates for a free-text ingredient name, most-specific first:
/// the full lowercased name, then its last whitespace token, then the last
/// hyphen segment of that token ("all-purpose flour" -> "flour").
pub fn name_candidates(name: &str) -> Vec<String> {
    let n = name.to_lowercase();
    let mut out = vec![n.clone()];
    if let Some(last) = n.split_whitespace().last() {
        let last = last.trim_matches('-').to_string();
        if last != n {
            out.push(last);
        }
    }
    if let Some(last) = n.split_whitespace().last() {
        if let Some(seg) = last.split('-').next_back() {
            if !out.iter().any(|x| x == seg) {
                out.push(seg.to_string());
            }
        }
    }
    out
}

/// True if every whitespace-separated token of `s` is a size/quality descriptor.
pub fn is_all_descriptors(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.split_whitespace()
            .all(|w| SINGLE_DESCRIPTORS.contains(&w.to_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_size_descriptors() {
        assert_eq!(normalize_ingredient_name("large eggs"), "eggs");
        assert_eq!(normalize_ingredient_name("Extra Large Eggs"), "eggs");
        assert_eq!(
            normalize_ingredient_name("boneless skinless chicken breast"),
            "chicken breast"
        );
    }

    #[test]
    fn keeps_meaningful_leading_words() {
        assert_eq!(
            normalize_ingredient_name("all-purpose flour"),
            "all-purpose flour"
        );
        assert_eq!(
            normalize_ingredient_name("red bell pepper"),
            "red bell pepper"
        );
        assert_eq!(normalize_ingredient_name("ground beef"), "ground beef");
    }

    #[test]
    fn descriptor_only_name_is_preserved() {
        assert_eq!(normalize_ingredient_name("large"), "large");
    }

    #[test]
    fn key_aggregates_across_descriptor() {
        assert_eq!(
            IngredientKey::new("large eggs", UnitKind::Count),
            IngredientKey::new("eggs", UnitKind::Count)
        );
    }

    #[test]
    fn is_all_descriptors_detects_descriptor_lists() {
        assert!(is_all_descriptors("skinless"));
        assert!(is_all_descriptors("boneless skinless"));
        assert!(!is_all_descriptors("chicken breast"));
        assert!(!is_all_descriptors("boneless chicken"));
        assert!(!is_all_descriptors(""));
    }
}
