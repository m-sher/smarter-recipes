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

#[derive(Debug, Clone)]
struct Combo {
    /// index into packages → count
    counts: Vec<u32>,
    total_size: f64,
    total_cost: Option<u64>,
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

    let mut pkgs: Vec<&Package> = packages.iter().filter(|p| p.size_canonical > 0.0).collect();
    if pkgs.is_empty() {
        return (vec![], 0.0, None);
    }
    // Prefer considering smaller packages first for leftover minimization
    pkgs.sort_by(|a, b| a.size_canonical.partial_cmp(&b.size_canonical).unwrap());

    let mut best: Option<Combo> = None;

    // DFS over package multiset counts
    #[allow(clippy::too_many_arguments)]
    fn search(
        pkgs: &[&Package],
        idx: usize,
        required: f64,
        opts: &OptimizeOptions,
        counts: &mut Vec<u32>,
        size: f64,
        cost: Option<u64>,
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
                total_cost: cost,
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
        let max_c = if p.size_canonical >= required && size < required {
            ((required - size) / p.size_canonical).ceil() as u32 + 1
        } else {
            ((required - size).max(0.0) / p.size_canonical).ceil() as u32 + 2
        };
        let max_c = max_c.min(opts.max_packages.saturating_sub(n));

        for c in 0..=max_c {
            counts[idx] = c;
            let add_size = p.size_canonical * f64::from(c);
            let new_cost = if c == 0 {
                cost
            } else {
                match (cost, p.price_cents.map(|pc| pc * u64::from(c))) {
                    (Some(a), Some(b)) => Some(a + b),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                }
            };
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
        None,
        0,
        &mut best,
    );

    // Fallback: single largest packages until covered
    let combo = best.unwrap_or_else(|| {
        let mut counts = vec![0u32; pkgs.len()];
        let mut size = 0.0;
        let mut cost = None;
        let mut n = 0;
        let largest = pkgs.len() - 1;
        while size < required && n < opts.max_packages {
            counts[largest] += 1;
            size += pkgs[largest].size_canonical;
            n += 1;
            if let Some(pc) = pkgs[largest].price_cents {
                cost = Some(cost.unwrap_or(0) + pc);
            }
        }
        Combo {
            counts,
            total_size: size,
            total_cost: cost,
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
    (picks, combo.total_size, combo.total_cost)
}

fn is_better(c: &Combo, best: &Option<Combo>) -> bool {
    let Some(b) = best else {
        return true;
    };
    // Primary: cost (missing cost ranks after known cost only if both cover —
    // treat missing as equal for both)
    match (c.total_cost, b.total_cost) {
        (Some(ca), Some(cb)) if ca != cb => return ca < cb,
        (Some(_), None) => return true,
        (None, Some(_)) => return false,
        _ => {}
    }
    let leftover_c = c.total_size; // compared via leftover below with required implicit in size
    let leftover_b = b.total_size;
    // Secondary: smaller total size (less leftover) — both >= required
    if (leftover_c - leftover_b).abs() > 1e-6 {
        return leftover_c < leftover_b;
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
        // Skip pure "to taste" / presence-only lines with no quantity.
        if *required <= 0.0 {
            continue;
        }
        let packages = catalog.packages_for(key);
        let (picks, purchased, cost) = optimize_purchase(*required, &packages, opts);
        let leftover = (purchased - required).max(0.0);
        let threshold = opts
            .leftover_abs_eps
            .max(opts.leftover_rel_eps * required.abs());
        let flagged = leftover > threshold;

        if let Some(c) = cost {
            if let Some(ref mut t) = total_cost {
                *t += c;
            }
        } else if !picks.is_empty() {
            // Purchased something without a known price → total incomplete.
            total_cost = None;
        }

        items.push(ShoppingItem {
            ingredient: key.clone(),
            required_canonical: *required,
            required_unit_label: ShoppingList::kind_label(key.kind).to_string(),
            packages: picks,
            purchased_canonical: purchased,
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
        // 14 oz needed → 16 oz has less leftover than 32 oz
        // Even if we only look at leftover when comparing... both have costs;
        // 16oz costs 199, 32oz costs 349 → cost primary picks 16oz anyway.
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
        // 32 oz needed: 2x16 = 398 cents, 1x32 = 349 cents → prefer 32
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
        let (picks, purchased, _) = optimize_purchase(req, &pkgs, &OptimizeOptions::default());
        assert!((picks[0].size_canonical - oz_mass(16.0)).abs() < 0.01);
        assert!(purchased - req < oz_mass(3.0));
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
}
