//! Offline package catalog keyed by ingredient name patterns.

use crate::domain::{IngredientKey, UnitKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Package {
    pub label: String,
    /// Size in canonical units for `kind` (g, ml, or ea).
    pub size_canonical: f64,
    /// Price in cents for one package, if known.
    pub price_cents: Option<u64>,
    pub kind: UnitKind,
}

#[derive(Debug, Clone, Default)]
pub struct PackageCatalog {
    /// Exact ingredient name → packages
    exact: HashMap<String, Vec<Package>>,
    /// Default packages by unit kind when no name match.
    by_kind: HashMap<UnitKind, Vec<Package>>,
}

impl PackageCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sensible US grocery defaults for common ingredients (canonical units).
    pub fn with_defaults() -> Self {
        let mut c = Self::new();

        // Milk: often sold by fluid volume; we also offer mass-oz style for plan examples.
        c.insert(
            "milk",
            vec![
                Package {
                    label: "16 fl oz milk".into(),
                    size_canonical: 473.176, // 16 fl oz in ml
                    price_cents: Some(199),
                    kind: UnitKind::Volume,
                },
                Package {
                    label: "32 fl oz milk".into(),
                    size_canonical: 946.353,
                    price_cents: Some(349),
                    kind: UnitKind::Volume,
                },
                Package {
                    label: "1/2 gallon milk".into(),
                    size_canonical: 1892.71,
                    price_cents: Some(299),
                    kind: UnitKind::Volume,
                },
            ],
        );
        // Mass-oriented milk entries for recipes using oz/lb
        c.exact.insert(
            "milk#mass".into(),
            vec![
                Package {
                    label: "16 oz milk".into(),
                    size_canonical: 16.0 * 28.349523125,
                    price_cents: Some(199),
                    kind: UnitKind::Mass,
                },
                Package {
                    label: "32 oz milk".into(),
                    size_canonical: 32.0 * 28.349523125,
                    price_cents: Some(349),
                    kind: UnitKind::Mass,
                },
            ],
        );

        for (name, packs) in [
            (
                "eggs",
                vec![
                    Package {
                        label: "dozen eggs".into(),
                        size_canonical: 12.0,
                        price_cents: Some(349),
                        kind: UnitKind::Count,
                    },
                    Package {
                        label: "half-dozen eggs".into(),
                        size_canonical: 6.0,
                        price_cents: Some(199),
                        kind: UnitKind::Count,
                    },
                ],
            ),
            (
                "flour",
                vec![
                    Package {
                        label: "2 lb flour".into(),
                        size_canonical: 2.0 * 453.59237,
                        price_cents: Some(249),
                        kind: UnitKind::Mass,
                    },
                    Package {
                        label: "5 lb flour".into(),
                        size_canonical: 5.0 * 453.59237,
                        price_cents: Some(449),
                        kind: UnitKind::Mass,
                    },
                ],
            ),
            (
                "butter",
                vec![
                    Package {
                        label: "1 lb butter".into(),
                        size_canonical: 453.59237,
                        price_cents: Some(499),
                        kind: UnitKind::Mass,
                    },
                    Package {
                        label: "4 sticks butter (1 lb)".into(),
                        size_canonical: 453.59237,
                        price_cents: Some(529),
                        kind: UnitKind::Mass,
                    },
                ],
            ),
            (
                "sugar",
                vec![
                    Package {
                        label: "2 lb sugar".into(),
                        size_canonical: 2.0 * 453.59237,
                        price_cents: Some(229),
                        kind: UnitKind::Mass,
                    },
                    Package {
                        label: "4 lb sugar".into(),
                        size_canonical: 4.0 * 453.59237,
                        price_cents: Some(399),
                        kind: UnitKind::Mass,
                    },
                ],
            ),
            (
                "rice",
                vec![
                    Package {
                        label: "1 lb rice".into(),
                        size_canonical: 453.59237,
                        price_cents: Some(149),
                        kind: UnitKind::Mass,
                    },
                    Package {
                        label: "5 lb rice".into(),
                        size_canonical: 5.0 * 453.59237,
                        price_cents: Some(599),
                        kind: UnitKind::Mass,
                    },
                ],
            ),
            (
                "onion",
                vec![Package {
                    label: "onion (each)".into(),
                    size_canonical: 1.0,
                    price_cents: Some(79),
                    kind: UnitKind::Count,
                }],
            ),
            (
                "garlic",
                vec![Package {
                    label: "garlic bulb".into(),
                    size_canonical: 1.0,
                    price_cents: Some(69),
                    kind: UnitKind::Count,
                }],
            ),
        ] {
            c.insert(name, packs);
        }

        // Volume flour/sugar when recipes use cups (approximate density not applied;
        // we stock volume packages too for cup-based recipes).
        c.exact.insert(
            "flour#volume".into(),
            vec![
                Package {
                    label: "2 lb flour (~7.5 cups)".into(),
                    size_canonical: 7.5 * 236.5882365,
                    price_cents: Some(249),
                    kind: UnitKind::Volume,
                },
                Package {
                    label: "5 lb flour (~18 cups)".into(),
                    size_canonical: 18.0 * 236.5882365,
                    price_cents: Some(449),
                    kind: UnitKind::Volume,
                },
            ],
        );
        c.exact.insert(
            "sugar#volume".into(),
            vec![Package {
                label: "2 lb sugar (~4.5 cups)".into(),
                size_canonical: 4.5 * 236.5882365,
                price_cents: Some(229),
                kind: UnitKind::Volume,
            }],
        );
        c.exact.insert(
            "butter#volume".into(),
            vec![Package {
                label: "1 lb butter (2 cups)".into(),
                size_canonical: 2.0 * 236.5882365,
                price_cents: Some(499),
                kind: UnitKind::Volume,
            }],
        );

        // Generic fallbacks by kind
        c.by_kind.insert(
            UnitKind::Mass,
            vec![
                Package {
                    label: "8 oz package".into(),
                    size_canonical: 8.0 * 28.349523125,
                    price_cents: Some(199),
                    kind: UnitKind::Mass,
                },
                Package {
                    label: "16 oz package".into(),
                    size_canonical: 16.0 * 28.349523125,
                    price_cents: Some(299),
                    kind: UnitKind::Mass,
                },
                Package {
                    label: "32 oz package".into(),
                    size_canonical: 32.0 * 28.349523125,
                    price_cents: Some(499),
                    kind: UnitKind::Mass,
                },
            ],
        );
        c.by_kind.insert(
            UnitKind::Volume,
            vec![
                Package {
                    label: "8 fl oz".into(),
                    size_canonical: 8.0 * 29.5735295625,
                    price_cents: Some(149),
                    kind: UnitKind::Volume,
                },
                Package {
                    label: "16 fl oz".into(),
                    size_canonical: 16.0 * 29.5735295625,
                    price_cents: Some(199),
                    kind: UnitKind::Volume,
                },
                Package {
                    label: "32 fl oz".into(),
                    size_canonical: 32.0 * 29.5735295625,
                    price_cents: Some(299),
                    kind: UnitKind::Volume,
                },
            ],
        );
        c.by_kind.insert(
            UnitKind::Count,
            vec![
                Package {
                    label: "each".into(),
                    size_canonical: 1.0,
                    price_cents: Some(99),
                    kind: UnitKind::Count,
                },
                Package {
                    label: "pack of 6".into(),
                    size_canonical: 6.0,
                    price_cents: Some(499),
                    kind: UnitKind::Count,
                },
            ],
        );
        c.by_kind.insert(
            UnitKind::Other,
            vec![Package {
                label: "1 unit".into(),
                size_canonical: 1.0,
                price_cents: None,
                kind: UnitKind::Other,
            }],
        );

        c
    }

    pub fn insert(&mut self, name: &str, packages: Vec<Package>) {
        self.exact.insert(name.to_lowercase(), packages);
    }

    pub fn packages_for(&self, key: &IngredientKey) -> Vec<Package> {
        let name = key.name.to_lowercase();
        // Try kind-specific override
        let kind_key = format!(
            "{}#{}",
            name,
            match key.kind {
                UnitKind::Mass => "mass",
                UnitKind::Volume => "volume",
                UnitKind::Count => "count",
                UnitKind::Other => "other",
            }
        );
        if let Some(p) = self.exact.get(&kind_key) {
            return p.clone();
        }
        // Exact name
        if let Some(p) = self.exact.get(&name) {
            let filtered: Vec<_> = p.iter().filter(|x| x.kind == key.kind).cloned().collect();
            if !filtered.is_empty() {
                return filtered;
            }
            // If kinds don't match, still return name packages only if kind matches any
        }
        // Token / whole-word match on catalog keys (avoid "milk" matching "buttermilk")
        for (k, packs) in &self.exact {
            if k.contains('#') {
                continue;
            }
            let word_match = name.split_whitespace().any(|t| t == k)
                || k.split_whitespace().any(|t| t == name.as_str())
                || name == *k;
            if word_match {
                let filtered: Vec<_> = packs
                    .iter()
                    .filter(|x| x.kind == key.kind)
                    .cloned()
                    .collect();
                if !filtered.is_empty() {
                    return filtered;
                }
            }
        }
        self.by_kind.get(&key.kind).cloned().unwrap_or_else(|| {
            vec![Package {
                label: "1 unit".into(),
                size_canonical: 1.0,
                price_cents: None,
                kind: key.kind,
            }]
        })
    }

    /// Load additional entries from a JSON file: `{ "ingredient": [Package, ...] }`
    pub fn load_json_overlay(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        let text = std::fs::read_to_string(path)?;
        let map: HashMap<String, Vec<Package>> = serde_json::from_str(&text)?;
        for (k, v) in map {
            self.exact.insert(k.to_lowercase(), v);
        }
        Ok(())
    }
}
