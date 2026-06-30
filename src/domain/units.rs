//! Unit kinds and concrete units used for quantity normalization.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Physical dimension of a unit. Only quantities of the same kind can be summed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Mass,
    Volume,
    Count,
    /// Unrecognized or unconvertible unit; keep as opaque.
    Other,
}

/// A measurement unit, optionally carrying a conversion factor to a canonical base.
///
/// Canonical bases:
/// - Mass: grams (g)
/// - Volume: milliliters (ml)
/// - Count: each (ea)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unit {
    pub name: String,
    pub kind: UnitKind,
    /// Multiply quantity by this to get the canonical base unit.
    pub to_base: f64,
}

impl Unit {
    pub fn new(name: impl Into<String>, kind: UnitKind, to_base: f64) -> Self {
        Self {
            name: name.into(),
            kind,
            to_base,
        }
    }

    pub fn to_canonical(&self, qty: f64) -> f64 {
        qty * self.to_base
    }

    pub fn from_canonical(&self, base_qty: f64) -> f64 {
        if self.to_base == 0.0 {
            base_qty
        } else {
            base_qty / self.to_base
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}
