//! Unit tables and alias resolution.

use crate::domain::{Unit, UnitKind};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::RwLock;

pub const CANONICAL_MASS: &str = "g";
pub const CANONICAL_VOLUME: &str = "ml";

/// Built-in units with conversion factors to canonical bases (g / ml / ea).
fn builtin_units() -> HashMap<String, Unit> {
    let mut m = HashMap::new();
    let add = |m: &mut HashMap<String, Unit>, names: &[&str], kind: UnitKind, to_base: f64| {
        for n in names {
            m.insert(n.to_lowercase(), Unit::new((*n).to_string(), kind, to_base));
        }
    };

    // Mass → grams
    add(&mut m, &["g", "gram", "grams", "gr"], UnitKind::Mass, 1.0);
    add(
        &mut m,
        &["kg", "kilogram", "kilograms"],
        UnitKind::Mass,
        1000.0,
    );
    add(
        &mut m,
        &["mg", "milligram", "milligrams"],
        UnitKind::Mass,
        0.001,
    );
    add(
        &mut m,
        &["oz", "ounce", "ounces"],
        UnitKind::Mass,
        28.349523125,
    );
    add(
        &mut m,
        &["lb", "lbs", "pound", "pounds"],
        UnitKind::Mass,
        453.59237,
    );
    // US customary volume often used for mass-like ingredients still treated as volume below.

    // Volume → milliliters
    add(
        &mut m,
        &[
            "ml",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitres",
        ],
        UnitKind::Volume,
        1.0,
    );
    add(
        &mut m,
        &["l", "liter", "liters", "litre", "litres"],
        UnitKind::Volume,
        1000.0,
    );
    add(
        &mut m,
        &["tsp", "teaspoon", "teaspoons", "t"],
        UnitKind::Volume,
        4.92892159375,
    );
    add(
        &mut m,
        &["tbsp", "tablespoon", "tablespoons", "T", "tbl", "tbs"],
        UnitKind::Volume,
        14.78676478125,
    );
    add(&mut m, &["cup", "cups", "c"], UnitKind::Volume, 236.5882365);
    add(
        &mut m,
        &["fl oz", "floz", "fluid ounce", "fluid ounces"],
        UnitKind::Volume,
        29.5735295625,
    );
    add(
        &mut m,
        &["pt", "pint", "pints"],
        UnitKind::Volume,
        473.176473,
    );
    add(
        &mut m,
        &["qt", "quart", "quarts"],
        UnitKind::Volume,
        946.352946,
    );
    add(
        &mut m,
        &["gal", "gallon", "gallons"],
        UnitKind::Volume,
        3785.411784,
    );
    add(&mut m, &["pinch", "pinches"], UnitKind::Volume, 0.308057599);
    add(&mut m, &["dash", "dashes"], UnitKind::Volume, 0.616115199);

    // Count
    add(
        &mut m,
        &[
            "ea", "each", "whole", "piece", "pieces", "pc", "pcs", "item", "items", "clove",
            "cloves", "slice", "slices", "can", "cans", "bunch", "bunches", "head", "heads",
            "stalk", "stalks", "sprig", "sprigs", "leaf", "leaves", "packet", "packets", "package",
            "packages", "bag", "bags", "jar", "jars", "bottle", "bottles",
        ],
        UnitKind::Count,
        1.0,
    );

    m
}

static UNITS: Lazy<RwLock<HashMap<String, Unit>>> = Lazy::new(|| RwLock::new(builtin_units()));

/// Look up a unit by alias (case-insensitive). Returns a clone of the registered unit.
pub fn lookup_unit(name: &str) -> Option<Unit> {
    let key = name.trim().to_lowercase();
    // Prefer exact key; also try without trailing 's' for simple plurals if not found.
    let map = UNITS.read().ok()?;
    if let Some(u) = map.get(&key) {
        return Some(u.clone());
    }
    // "fl. oz." style cleanup
    let cleaned = key
        .replace('.', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(u) = map.get(&cleaned) {
        return Some(u.clone());
    }
    None
}

/// Resolve a unit token to a [`Unit`], or treat unknown tokens as Other with factor 1.
pub fn resolve_unit(name: &str) -> Unit {
    lookup_unit(name).unwrap_or_else(|| Unit::new(name.to_string(), UnitKind::Other, 1.0))
}

/// Register an additional alias at runtime (tests / user config).
pub fn register_alias(alias: &str, unit: Unit) {
    if let Ok(mut map) = UNITS.write() {
        map.insert(alias.trim().to_lowercase(), unit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mass_conversions() {
        let oz = lookup_unit("oz").unwrap();
        assert_eq!(oz.kind, UnitKind::Mass);
        let grams = oz.to_canonical(16.0);
        assert!((grams - 453.592).abs() < 0.01);
    }

    #[test]
    fn volume_conversions() {
        let cup = lookup_unit("cup").unwrap();
        assert_eq!(cup.kind, UnitKind::Volume);
        assert!((cup.to_canonical(1.0) - 236.588).abs() < 0.01);
    }

    #[test]
    fn case_insensitive() {
        assert!(lookup_unit("TBSP").is_some());
        assert!(lookup_unit("Cups").is_some());
    }
}
