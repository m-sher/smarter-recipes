use crate::domain::{IngredientKey, PackagePick, ShoppingItem, ShoppingList};
use crate::pricing::{Package, PackageCatalog};

#[derive(Debug, Clone)]
pub struct OptimizeOptions {
    pub max_packages: u32,
    /// Leftover is flagged when greater than max(abs_eps, rel_eps * required).
    pub leftover_abs_eps: f64,
    pub leftover_rel_eps: f64,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            max_packages: 12,
            leftover_abs_eps: 1.0,
            leftover_rel_eps: 0.01,
        }
    }
}

/// Running cost of a package combination.
/// `Empty` is the identity (no packages yet); `Unknown` means ≥1 package lacked a price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunCost {
    Empty,
    Known(u64),
    Unknown,
}

impl RunCost {
    fn add_package(self, unit_price: Option<u64>, count: u32) -> Self {
        if count == 0 {
            return self;
        }
        match (self, unit_price) {
            (_, None) => RunCost::Unknown,
            (RunCost::Unknown, _) => RunCost::Unknown,
            (RunCost::Empty, Some(p)) => RunCost::Known(p * u64::from(count)),
            (RunCost::Known(a), Some(p)) => RunCost::Known(a + p * u64::from(count)),
        }
    }

    fn as_option(self) -> Option<u64> {
        match self {
            RunCost::Known(c) => Some(c),
            RunCost::Empty | RunCost::Unknown => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Combo {
    counts: Vec<u32>,
    total_size: f64,
    cost: RunCost,
    n_packages: u32,
}

/// Choose packages covering `required` canonical units.
pub fn optimize_purchase(
    required: f64,
    packages: &[Package],
    opts: &OptimizeOptions,
) -> (Vec<PackagePick>, f64, Option<u64>) {
    if required <= 0.0 || packages.is_empty() {
        return (vec![], 0.0, None);
    }

    let mut pkgs: Vec<&Package> = packages
        .iter()
        .filter(|p| p.size_canonical.is_finite() && p.size_canonical > 0.0)
        .collect();
    if pkgs.is_empty() {
        return (vec![], 0.0, None);
    }
    pkgs.sort_by(|a, b| {
        a.size_canonical
            .partial_cmp(&b.size_canonical)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut best: Option<Combo> = None;

    #[allow(clippy::too_many_arguments)]
    fn search(
        pkgs: &[&Package],
        idx: usize,
        required: f64,
        opts: &OptimizeOptions,
        counts: &mut Vec<u32>,
        size: f64,
        cost: RunCost,
        n: u32,
        best: &mut Option<Combo>,
    ) {
        if n > opts.max_packages {
            return;
        }
        if size + 1e-9 >= required {
            let combo = Combo {
                counts: counts.clone(),
                total_size: size,
                cost,
                n_packages: n,
            };
            if is_better(&combo, best) {
                *best = Some(combo);
            }
        }
        if idx >= pkgs.len() {
            return;
        }

        let p = pkgs[idx];
        // Saturating count arithmetic; the float→u32 cast may reach u32::MAX.
        let max_c = if p.size_canonical >= required && size < required {
            (((required - size) / p.size_canonical).ceil() as u32).saturating_add(1)
        } else {
            (((required - size).max(0.0) / p.size_canonical).ceil() as u32).saturating_add(2)
        };
        let max_c = max_c.min(opts.max_packages.saturating_sub(n));

        for c in 0..=max_c {
            counts[idx] = c;
            let add_size = p.size_canonical * f64::from(c);
            let new_cost = cost.add_package(p.price_cents, c);
            search(
                pkgs,
                idx + 1,
                required,
                opts,
                counts,
                size + add_size,
                new_cost,
                n + c,
                best,
            );
        }
        counts[idx] = 0;
    }

    let mut counts = vec![0u32; pkgs.len()];
    search(
        &pkgs,
        0,
        required,
        opts,
        &mut counts,
        0.0,
        RunCost::Empty,
        0,
        &mut best,
    );

    let combo = best.unwrap_or_else(|| {
        let mut counts = vec![0u32; pkgs.len()];
        let mut size = 0.0;
        let mut cost = RunCost::Empty;
        let mut n = 0;
        let largest = pkgs.len() - 1;
        while size < required && n < opts.max_packages {
            counts[largest] += 1;
            size += pkgs[largest].size_canonical;
            n += 1;
            cost = cost.add_package(pkgs[largest].price_cents, 1);
        }
        Combo {
            counts,
            total_size: size,
            cost,
            n_packages: n,
        }
    });

    let mut picks = Vec::new();
    for (i, &c) in combo.counts.iter().enumerate() {
        if c > 0 {
            let p = pkgs[i];
            picks.push(PackagePick {
                label: p.label.clone(),
                size_canonical: p.size_canonical,
                count: c,
                unit_price_cents: p.price_cents,
            });
        }
    }
    (picks, combo.total_size, combo.cost.as_option())
}

fn is_better(c: &Combo, best: &Option<Combo>) -> bool {
    let Some(b) = best else {
        return true;
    };
    // Primary: known cost beats unknown; lower known cost wins.
    match (c.cost, b.cost) {
        (RunCost::Known(ca), RunCost::Known(cb)) if ca != cb => return ca < cb,
        (RunCost::Known(_), RunCost::Unknown) => return true,
        (RunCost::Unknown, RunCost::Known(_)) => return false,
        (RunCost::Known(_), RunCost::Empty) => return true,
        (RunCost::Empty, RunCost::Known(_)) => return false,
        _ => {}
    }
    // Secondary: smaller total size (less leftover)
    if (c.total_size - b.total_size).abs() > 1e-6 {
        return c.total_size < b.total_size;
    }
    // Tertiary: fewer packages
    c.n_packages < b.n_packages
}

pub fn optimize_shopping_list(
    plan_id: &str,
    requirements: &[(IngredientKey, f64)],
    catalog: &PackageCatalog,
    opts: &OptimizeOptions,
) -> ShoppingList {
    let mut items = Vec::new();
    let mut total_cost: Option<u64> = Some(0);

    for (key, required) in requirements {
        if *required <= 0.0 {
            continue;
        }
        let (req_mass_or_vol, packages) = catalog.packages_for_requirement(key, *required);
        let (picks, purchased, cost) = optimize_purchase(req_mass_or_vol, &packages, opts);
        // Map purchased back for display in the requirement's kind when converted.
        let (display_required, display_purchased, display_unit) =
            catalog.display_amounts(key, *required, purchased);
        let leftover = (display_purchased - display_required).max(0.0);
        let threshold = opts
            .leftover_abs_eps
            .max(opts.leftover_rel_eps * display_required.abs());
        let flagged = leftover > threshold;

        if let Some(c) = cost {
            if let Some(ref mut t) = total_cost {
                *t += c;
            }
        } else if !picks.is_empty() {
            total_cost = None;
        }

        items.push(ShoppingItem {
            ingredient: key.clone(),
            required_canonical: display_required,
            required_unit_label: display_unit,
            packages: picks,
            purchased_canonical: display_purchased,
            leftover_canonical: leftover,
            total_cost_cents: cost,
            leftover_flagged: flagged,
        });
    }

    items.sort_by(|a, b| a.ingredient.name.cmp(&b.ingredient.name));

    ShoppingList {
        plan_id: plan_id.to_string(),
        items,
        total_cost_cents: total_cost,
    }
}

/// Helper for tests: oz mass package sizes in grams.
#[cfg(test)]
pub fn oz_mass(oz: f64) -> f64 {
    oz * 28.349523125
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::UnitKind;
    use crate::pricing::Package;

    fn milk_packages() -> Vec<Package> {
        vec![
            Package {
                label: "16 oz milk".into(),
                size_canonical: oz_mass(16.0),
                price_cents: Some(199),
                kind: UnitKind::Mass,
            },
            Package {
                label: "32 oz milk".into(),
                size_canonical: oz_mass(32.0),
                price_cents: Some(349),
                kind: UnitKind::Mass,
            },
        ]
    }

    #[test]
    fn prefer_less_leftover_when_costs_differ_secondary() {
        let req = oz_mass(14.0);
        let (picks, purchased, cost) =
            optimize_purchase(req, &milk_packages(), &OptimizeOptions::default());
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].count, 1);
        assert!((picks[0].size_canonical - oz_mass(16.0)).abs() < 0.01);
        assert!(purchased >= req);
        assert_eq!(cost, Some(199));
        let leftover = purchased - req;
        assert!(leftover < oz_mass(3.0));
    }

    #[test]
    fn prefer_cheaper_when_zero_leftover() {
        let req = oz_mass(32.0);
        let (picks, purchased, cost) =
            optimize_purchase(req, &milk_packages(), &OptimizeOptions::default());
        assert!((purchased - req).abs() < 0.01);
        assert_eq!(cost, Some(349));
        assert_eq!(picks.len(), 1);
        assert!((picks[0].size_canonical - oz_mass(32.0)).abs() < 0.01);
    }

    #[test]
    fn prefer_less_leftover_equal_missing_prices() {
        let pkgs = vec![
            Package {
                label: "16 oz".into(),
                size_canonical: oz_mass(16.0),
                price_cents: None,
                kind: UnitKind::Mass,
            },
            Package {
                label: "32 oz".into(),
                size_canonical: oz_mass(32.0),
                price_cents: None,
                kind: UnitKind::Mass,
            },
        ];
        let req = oz_mass(14.0);
        let (picks, purchased, cost) = optimize_purchase(req, &pkgs, &OptimizeOptions::default());
        assert!((picks[0].size_canonical - oz_mass(16.0)).abs() < 0.01);
        assert!(purchased - req < oz_mass(3.0));
        assert_eq!(cost, None);
    }

    #[test]
    fn ignores_non_finite_package_sizes() {
        let pkgs = vec![
            Package {
                label: "nan".into(),
                size_canonical: f64::NAN,
                price_cents: Some(50),
                kind: UnitKind::Mass,
            },
            Package {
                label: "inf".into(),
                size_canonical: f64::INFINITY,
                price_cents: Some(50),
                kind: UnitKind::Mass,
            },
            Package {
                label: "100g".into(),
                size_canonical: 100.0,
                price_cents: Some(50),
                kind: UnitKind::Mass,
            },
        ];
        let (picks, purchased, _) = optimize_purchase(150.0, &pkgs, &OptimizeOptions::default());
        assert!(picks.iter().all(|p| p.label == "100g"));
        assert!(purchased.is_finite() && purchased >= 150.0);
    }

    #[test]
    fn tiny_package_huge_requirement_does_not_overflow() {
        // ratio far exceeds u32::MAX; must not panic on the count arithmetic.
        let pkgs = vec![Package {
            label: "tiny".into(),
            size_canonical: 1e-6,
            price_cents: Some(1),
            kind: UnitKind::Mass,
        }];
        let (_picks, _purchased, _cost) =
            optimize_purchase(5000.0, &pkgs, &OptimizeOptions::default());
    }

    #[test]
    fn multi_pack_cover() {
        let pkgs = vec![Package {
            label: "100g".into(),
            size_canonical: 100.0,
            price_cents: Some(50),
            kind: UnitKind::Mass,
        }];
        let (picks, purchased, cost) = optimize_purchase(250.0, &pkgs, &OptimizeOptions::default());
        assert_eq!(picks[0].count, 3);
        assert!((purchased - 300.0).abs() < 0.01);
        assert_eq!(cost, Some(150));
    }

    #[test]
    fn mixed_priced_and_unpriced_reports_unknown_cost() {
        let pkgs = vec![
            Package {
                label: "20u priced".into(),
                size_canonical: 20.0,
                price_cents: Some(100),
                kind: UnitKind::Count,
            },
            Package {
                label: "20u free?".into(),
                size_canonical: 20.0,
                price_cents: None,
                kind: UnitKind::Count,
            },
        ];
        // Need 30: only feasible with both types or 2x priced.
        // 2x priced = 200 cost, 0 leftover from 40; 1 priced + 1 unpriced = unknown cost, 10 leftover.
        // Known cost 200 should beat unknown.
        let (picks, _purchased, cost) = optimize_purchase(30.0, &pkgs, &OptimizeOptions::default());
        assert_eq!(cost, Some(200));
        assert!(picks.iter().all(|p| p.unit_price_cents.is_some()));

        // Force only unpriced by using packages where priced can't cover alone efficiently...
        // Need 20 with only mixed - use 1 unpriced only possible with size 20 unpriced
        let only_mixed_needed = vec![
            Package {
                label: "10 priced".into(),
                size_canonical: 10.0,
                price_cents: Some(50),
                kind: UnitKind::Count,
            },
            Package {
                label: "25 unpriced".into(),
                size_canonical: 25.0,
                price_cents: None,
                kind: UnitKind::Count,
            },
        ];
        // Need 25: 1x unpriced (unknown cost, 0 leftover) vs 3x priced (150, 5 leftover)
        // Known cost should win even with leftover
        let (_picks, _, cost) =
            optimize_purchase(25.0, &only_mixed_needed, &OptimizeOptions::default());
        assert_eq!(cost, Some(150));

        // Need 26: 3x priced = 30 (cost 150), or 1 unpriced doesn't cover... 1 unpriced + 1 priced = unknown
        // Known 200 for 4x10=40
        let (_p, _, cost) =
            optimize_purchase(26.0, &only_mixed_needed, &OptimizeOptions::default());
        assert!(cost.is_some());

        // Only unpriced packages available that cover: cost must be None
        let unpriced_only = vec![Package {
            label: "30u".into(),
            size_canonical: 30.0,
            price_cents: None,
            kind: UnitKind::Count,
        }];
        let (_p, _, cost) = optimize_purchase(30.0, &unpriced_only, &OptimizeOptions::default());
        assert_eq!(cost, None);
    }
}
