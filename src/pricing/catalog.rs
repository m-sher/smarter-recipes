//! Offline package catalog keyed by ingredient name patterns.

use super::density::volume_ml_to_mass_g;
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
    /// Exact ingredient name → packages (and `name#kind` overrides)
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

        c.insert(
            "milk",
            vec![
                Package {
                    label: "16 fl oz milk".into(),
                    size_canonical: 473.176,
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
                vec![Package {
                    label: "1 lb butter".into(),
                    size_canonical: 453.59237,
                    price_cents: Some(499),
                    kind: UnitKind::Mass,
                }],
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
                "salt",
                vec![Package {
                    label: "26 oz salt".into(),
                    size_canonical: 26.0 * 28.349523125,
                    price_cents: Some(129),
                    kind: UnitKind::Mass,
                }],
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

        // Kind-specific volume entries.
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

    fn kind_suffix(kind: UnitKind) -> &'static str {
        match kind {
            UnitKind::Mass => "mass",
            UnitKind::Volume => "volume",
            UnitKind::Count => "count",
            UnitKind::Other => "other",
        }
    }

    /// Resolve packages for an ingredient key without density conversion.
    pub fn packages_for(&self, key: &IngredientKey) -> Vec<Package> {
        self.packages_for_kind(&key.name, key.kind)
    }

    fn packages_for_kind(&self, name: &str, kind: UnitKind) -> Vec<Package> {
        let candidates = crate::domain::name_candidates(name);
        // Try name#kind for each candidate
        for cand in &candidates {
            let kind_key = format!("{}#{}", cand, Self::kind_suffix(kind));
            if let Some(p) = self.exact.get(&kind_key) {
                return p.clone();
            }
        }
        // Exact name entries filtered by kind
        for cand in &candidates {
            if let Some(p) = self.exact.get(cand) {
                let filtered: Vec<_> = p.iter().filter(|x| x.kind == kind).cloned().collect();
                if !filtered.is_empty() {
                    return filtered;
                }
            }
        }
        // Word-match on catalog keys (skip #kind keys here).
        let name_lc = name.to_lowercase();
        for (k, packs) in &self.exact {
            if k.contains('#') {
                continue;
            }
            let word_match = name_lc.split_whitespace().any(|t| t == k.as_str())
                || k.split_whitespace().any(|t| t == name_lc.as_str())
                || name_lc == *k
                || candidates.iter().any(|c| c == k);
            if word_match {
                let filtered: Vec<_> = packs.iter().filter(|x| x.kind == kind).cloned().collect();
                if !filtered.is_empty() {
                    return filtered;
                }
            }
        }
        self.by_kind.get(&kind).cloned().unwrap_or_else(|| {
            vec![Package {
                label: "1 unit".into(),
                size_canonical: 1.0,
                price_cents: None,
                kind,
            }]
        })
    }

    /// When true, recipe volume for this ingredient should be priced as mass (density known
    /// and mass packages available).
    fn uses_mass_via_density(&self, key: &IngredientKey) -> bool {
        key.kind == UnitKind::Volume
            && volume_ml_to_mass_g(&key.name, 1.0).is_some()
            && self
                .packages_for(&IngredientKey {
                    name: key.name.clone(),
                    kind: UnitKind::Mass,
                })
                .iter()
                .any(|p| p.kind == UnitKind::Mass)
    }

    /// Packages and required amount in the **package** measurement space.
    ///
    /// For volume-measured dry goods with a known density, converts required ml → g
    /// and returns mass packages.
    pub fn packages_for_requirement(
        &self,
        key: &IngredientKey,
        required: f64,
    ) -> (f64, Vec<Package>) {
        if self.uses_mass_via_density(key) {
            let grams = volume_ml_to_mass_g(&key.name, required).expect("density checked");
            let packs = self.packages_for(&IngredientKey {
                name: key.name.clone(),
                kind: UnitKind::Mass,
            });
            return (grams, packs);
        }
        (required, self.packages_for(key))
    }

    /// Map purchased amount in package space back for display.
    /// When volume→mass conversion was applied, amounts are shown in grams.
    pub fn display_amounts(
        &self,
        key: &IngredientKey,
        required_recipe: f64,
        purchased_package_space: f64,
    ) -> (f64, f64, String) {
        if self.uses_mass_via_density(key) {
            let grams_req =
                volume_ml_to_mass_g(&key.name, required_recipe).expect("density checked");
            return (grams_req, purchased_package_space, "g".into());
        }
        let label = match key.kind {
            UnitKind::Mass => "g",
            UnitKind::Volume => "ml",
            UnitKind::Count => "ea",
            UnitKind::Other => "unit",
        };
        (required_recipe, purchased_package_space, label.to_string())
    }

    /// Convert a shopping-list purchased amount (display / package units) into
    /// the requirement key's canonical units for pantry storage.
    ///
    /// Density-converted dry goods are optimized and displayed in grams but the
    /// recipe key remains volume (ml); this maps grams back to ml.
    pub fn purchased_to_key_units(&self, key: &IngredientKey, purchased_display: f64) -> f64 {
        if self.uses_mass_via_density(key) {
            crate::pricing::density::mass_g_to_volume_ml(&key.name, purchased_display)
                .unwrap_or(purchased_display)
        } else {
            purchased_display
        }
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

    /// Merge packages from an external source into the catalog (by ingredient name).
    pub fn merge_packages(&mut self, name: &str, packages: Vec<Package>) {
        let key = name.to_lowercase();
        self.exact
            .entry(key)
            .and_modify(|e| {
                e.extend(packages.clone());
            })
            .or_insert(packages);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiword_flour_volume_uses_mass_via_density() {
        let cat = PackageCatalog::with_defaults();
        let key = IngredientKey {
            name: "all-purpose flour".into(),
            kind: UnitKind::Volume,
        };
        let (req, packs) = cat.packages_for_requirement(&key, 473.176); // 2 cups
                                                                        // Should convert to grams and use mass flour packages
        assert!(req > 200.0 && req < 300.0, "req grams = {req}");
        assert!(packs.iter().all(|p| p.kind == UnitKind::Mass));
        assert!(packs
            .iter()
            .any(|p| p.label.contains("flour") || p.label.contains("lb")));
    }

    #[test]
    fn multiword_reaches_kind_specific_catalog() {
        let cat = PackageCatalog::with_defaults();
        let key = IngredientKey {
            name: "all-purpose flour".into(),
            kind: UnitKind::Volume,
        };
        // Direct kind lookup via candidates should find flour#volume if we ask packages_for
        let packs = cat.packages_for(&key);
        assert!(
            packs
                .iter()
                .any(|p| p.kind == UnitKind::Volume && p.label.contains("flour")),
            "got {:?}",
            packs.iter().map(|p| &p.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn single_word_flour_mass() {
        let cat = PackageCatalog::with_defaults();
        let key = IngredientKey {
            name: "flour".into(),
            kind: UnitKind::Mass,
        };
        let packs = cat.packages_for(&key);
        assert!(packs.iter().all(|p| p.kind == UnitKind::Mass));
    }

    #[test]
    fn salt_volume_converts_to_mass_packages() {
        let cat = PackageCatalog::with_defaults();
        let key = IngredientKey {
            name: "salt".into(),
            kind: UnitKind::Volume,
        };
        let tsp_ml = 4.92892159375;
        let (req, packs) = cat.packages_for_requirement(&key, 0.5 * tsp_ml);
        assert!(req < 5.0); // half tsp salt ~ 3g
        assert!(packs.iter().any(|p| p.kind == UnitKind::Mass));
    }

    #[test]
    fn sugar_volume_converts() {
        let cat = PackageCatalog::with_defaults();
        let key = IngredientKey {
            name: "sugar".into(),
            kind: UnitKind::Volume,
        };
        let (req, packs) = cat.packages_for_requirement(&key, 236.588); // 1 cup
        assert!(req > 150.0 && req < 250.0);
        assert!(packs.iter().all(|p| p.kind == UnitKind::Mass));
    }

    #[test]
    fn unknown_density_keeps_volume_packages() {
        let cat = PackageCatalog::with_defaults();
        let key = IngredientKey {
            name: "mystery powder".into(),
            kind: UnitKind::Volume,
        };
        let (req, packs) = cat.packages_for_requirement(&key, 100.0);
        assert!((req - 100.0).abs() < 1e-6);
        assert!(packs.iter().all(|p| p.kind == UnitKind::Volume));
    }
}
