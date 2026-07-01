//! Approximate densities (g/ml) for converting recipe volumes of dry goods to mass packages.
//!
//! Values are culinary approximations suitable for shopping estimates, not lab precision.

/// Grams per milliliter for a normalized ingredient name, if known.
pub fn density_g_per_ml(name: &str) -> Option<f64> {
    let n = name.to_lowercase();
    // Prefer longer / more specific keys first via manual checks.
    let table: &[(&str, f64)] = &[
        ("all-purpose flour", 0.53),
        ("all purpose flour", 0.53),
        ("bread flour", 0.53),
        ("whole wheat flour", 0.51),
        ("cake flour", 0.46),
        ("flour", 0.53),
        ("granulated sugar", 0.85),
        ("brown sugar", 0.93),
        ("powdered sugar", 0.56),
        ("confectioners sugar", 0.56),
        ("sugar", 0.85),
        ("butter", 0.911),
        ("salt", 1.217),
        ("kosher salt", 0.96),
        ("rice", 0.85),
        ("white rice", 0.85),
        ("brown rice", 0.82),
        ("oats", 0.34),
        ("rolled oats", 0.34),
        ("cocoa", 0.41),
        ("cocoa powder", 0.41),
        ("cornstarch", 0.54),
        ("baking powder", 0.90),
        ("baking soda", 0.95),
        ("honey", 1.42),
        ("oil", 0.92),
        ("olive oil", 0.91),
        ("vegetable oil", 0.92),
        ("milk", 1.03),
        ("buttermilk", 1.03),
        ("cream", 0.99),
        ("yogurt", 1.03),
        ("water", 1.0),
        ("vanilla", 0.88),
        ("vanilla extract", 0.88),
        ("cinnamon", 0.56),
        ("cumin", 0.48),
        ("paprika", 0.46),
        ("pepper", 0.45),
        ("black pepper", 0.45),
        ("cheese", 0.45),
        ("parmesan", 0.45),
        ("parsley", 0.20),
    ];

    // Exact match
    for (k, d) in table {
        if n == *k {
            return Some(*d);
        }
    }
    // Token / suffix match: "all-purpose flour" contains key "flour" as last token
    for (k, d) in table {
        if k.contains(' ') {
            continue; // multiword already tried exact
        }
        if n.split_whitespace().any(|t| t == *k) {
            return Some(*d);
        }
        // hyphenated last segment
        if n.split([' ', '-']).any(|t| t == *k) {
            return Some(*d);
        }
    }
    None
}

/// Convert volume (ml) to mass (g) using density, if known.
pub fn volume_ml_to_mass_g(name: &str, ml: f64) -> Option<f64> {
    density_g_per_ml(name).map(|d| ml * d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flour_density() {
        let d = density_g_per_ml("all-purpose flour").unwrap();
        assert!((d - 0.53).abs() < 0.01);
        let g = volume_ml_to_mass_g("flour", 236.588).unwrap(); // 1 cup
        assert!(g > 100.0 && g < 150.0);
    }

    #[test]
    fn sugar_and_salt() {
        assert!(density_g_per_ml("sugar").is_some());
        assert!(density_g_per_ml("salt").is_some());
    }

    #[test]
    fn unknown_returns_none() {
        assert!(density_g_per_ml("dragon scales").is_none());
    }
}
