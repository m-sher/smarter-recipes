//! Approximate densities (g/ml) for converting recipe volumes to mass.
//!
//! Values are culinary approximations (often derived from typical g-per-tsp
//! portion weights). Used by shopping package bridging and nutrition estimates.

/// Grams per milliliter for a normalized ingredient name, if known.
pub fn density_g_per_ml(name: &str) -> Option<f64> {
    let n = name.to_lowercase();
    // Prefer longer / more specific keys first (exact and multi-word match).
    let table: &[(&str, f64)] = &[
        // --- flours / sugars / leaveners ---
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
        ("cornstarch", 0.54),
        ("baking powder", 0.90),
        ("baking soda", 0.95),
        ("cocoa powder", 0.41),
        ("cocoa", 0.41),
        // --- dairy / fats / liquids ---
        ("butter", 0.911),
        ("ghee", 0.91),
        ("honey", 1.42),
        ("olive oil", 0.91),
        ("vegetable oil", 0.92),
        ("peanut oil", 0.91),
        ("sesame oil", 0.92),
        ("oil", 0.92),
        ("milk", 1.03),
        ("buttermilk", 1.03),
        ("cream", 0.99),
        ("yogurt", 1.03),
        ("water", 1.0),
        ("vanilla extract", 0.88),
        ("vanilla", 0.88),
        ("lemon juice", 1.03),
        ("lime juice", 1.03),
        ("tamarind paste", 1.15),
        ("ginger paste", 1.15),
        ("garlic paste", 1.15),
        ("ginger-garlic paste", 1.15),
        ("ginger garlic paste", 1.15),
        ("basic ginger-garlic paste", 1.15),
        ("tomato puree", 1.02),
        ("tomato purée", 1.02),
        // --- salt ---
        ("kosher salt", 0.96),
        ("salt", 1.217),
        // --- grains ---
        ("white rice", 0.85),
        ("brown rice", 0.82),
        ("rice", 0.85),
        ("rolled oats", 0.34),
        ("oats", 0.34),
        // --- ground spices / masalas (≈2–3 g per tsp → ~0.4–0.6 g/ml) ---
        ("garam masala", 0.48),
        ("garam masala powder", 0.48),
        ("chaat masala", 0.48),
        ("chat masala", 0.48),
        ("ground coriander", 0.40),
        ("coriander powder", 0.40),
        ("coriander seed powder", 0.40),
        ("ground turmeric", 0.55),
        ("turmeric powder", 0.55),
        ("turmeric", 0.55),
        ("ground cumin", 0.48),
        ("cumin powder", 0.48),
        ("ground asafoetida", 0.50),
        ("asafoetida", 0.50),
        ("hing", 0.50),
        ("ground ginger", 0.45),
        ("ginger powder", 0.45),
        ("garlic powder", 0.55),
        ("onion powder", 0.50),
        ("chili powder", 0.46),
        ("chilli powder", 0.46),
        ("red chili powder", 0.46),
        ("red chilli powder", 0.46),
        ("cayenne pepper", 0.46),
        ("red pepper flakes", 0.40),
        ("smoked paprika", 0.46),
        ("paprika", 0.46),
        ("ground nutmeg", 0.50),
        ("nutmeg", 0.50),
        ("ground cinnamon", 0.56),
        ("cinnamon", 0.56),
        ("ground black pepper", 0.45),
        ("black pepper", 0.45),
        ("pepper", 0.45),
        ("italian seasoning", 0.30),
        ("oregano", 0.30),
        ("thyme", 0.30),
        ("cumin", 0.48),
        // --- whole seeds (volume measures) ---
        ("cumin seeds", 0.50),
        ("whole cumin seeds", 0.50),
        ("black mustard seeds", 0.65),
        ("mustard seeds", 0.65),
        ("coriander seeds", 0.45),
        ("fenugreek seeds", 0.70),
        ("fennel seeds", 0.45),
        ("ajwain seeds", 0.45),
        ("carom seeds", 0.45),
        ("caraway seed", 0.45),
        ("caraway seeds", 0.45),
        ("sesame seeds", 0.62),
        ("black onion seeds", 0.55),
        ("nigella seeds", 0.55),
        ("kalonji", 0.55),
        ("black peppercorns", 0.50),
        ("peppercorns", 0.50),
        // --- fresh herbs (chopped, loosely packed) ---
        ("finely chopped fresh cilantro", 0.20),
        ("crudely chopped fresh cilantro", 0.20),
        ("chopped fresh cilantro", 0.20),
        ("fresh cilantro", 0.20),
        ("cilantro", 0.20),
        ("coriander leaves", 0.20),
        ("finely chopped coriander leaves", 0.20),
        ("fresh coriander", 0.20),
        ("coriander", 0.40), // often ground when used as powder-volume; seeds/ground covered above
        ("dried fenugreek leaves", 0.15),
        ("fenugreek leaves", 0.15),
        ("kasoori methi", 0.15),
        ("curry leaves", 0.12),
        ("parsley", 0.20),
        ("chopped cilantro", 0.20),
        ("minced garlic", 0.60),
        ("minced ginger", 0.55),
        ("peeled minced fresh ginger", 0.55),
        ("grated ginger", 0.55),
        ("fresh ginger", 0.55),
        ("ginger", 0.55),
        // --- cheese shreds ---
        ("parmesan", 0.45),
        ("cheese", 0.45),
    ];

    // 1) Exact match on full normalized name.
    for (k, d) in table {
        if n == *k {
            return Some(*d);
        }
    }
    // 2) Multi-word keys as substring / suffix (longer keys first in table order).
    for (k, d) in table {
        if !k.contains(' ') {
            continue;
        }
        if n.ends_with(k) || n.contains(k) {
            return Some(*d);
        }
    }
    // 3) Single-token keys: any whitespace/hyphen token equals the key.
    for (k, d) in table {
        if k.contains(' ') {
            continue;
        }
        if n.split_whitespace().any(|t| t == *k) {
            return Some(*d);
        }
        if n.split([' ', '-']).any(|t| t == *k) {
            return Some(*d);
        }
    }
    // 4) Spice-only fallbacks. Do **not** use a bare `ground ` prefix: that
    //    falsely assigns spice density to ground meats/nuts (ground beef,
    //    ground almonds, …), which corrupts nutrition coverage and shopping.
    //    Unlisted `ground X` spices must be added to the table explicitly.
    if n.contains("masala") {
        return Some(0.48);
    }
    // Culinary pastes are semi-solids near water density (~1.1–1.25 g/ml). This
    // intentionally covers tomato/curry/miso/almond/shrimp/etc. paste as well as
    // ginger-garlic paste; same shopping path as other densities.
    if n.ends_with(" paste") || n.contains("-garlic paste") || n.contains(" garlic paste") {
        return Some(1.15);
    }
    None
}

/// Convert volume (ml) to mass (g) using density, if known.
pub fn volume_ml_to_mass_g(name: &str, ml: f64) -> Option<f64> {
    density_g_per_ml(name).map(|d| ml * d)
}

/// Convert mass (g) to volume (ml) using density, if known.
pub fn mass_g_to_volume_ml(name: &str, g: f64) -> Option<f64> {
    density_g_per_ml(name).filter(|d| *d > 0.0).map(|d| g / d)
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
    fn spice_volumes_resolve() {
        // Names arrive already normalized (quantity stripped); these are clean keys.
        for name in [
            "garam masala",
            "ground turmeric",
            "ground coriander",
            "cumin seeds",
            "ginger paste",
            "finely chopped fresh cilantro",
            "garlic powder",
            "chaat masala",
            "black mustard seeds",
        ] {
            assert!(
                density_g_per_ml(name).is_some(),
                "expected density for {name}"
            );
        }
        // ~1 tsp (4.93 ml) garam masala → a few grams, not zero and not huge
        let g = volume_ml_to_mass_g("garam masala", 4.93).unwrap();
        assert!(g > 1.5 && g < 5.0, "garam masala tsp grams={g}");
    }

    #[test]
    fn masala_and_paste_fallback_not_ground_meat() {
        assert!(density_g_per_ml("some masala blend").is_some());
        assert!(density_g_per_ml("tomato paste").is_some()); // intentional paste ≈1.15
                                                             // Unlisted ground *spices* stay None (add them to the table explicitly).
        assert!(density_g_per_ml("ground mystery spice").is_none());
        // Ground meats/nuts must never get the spice density (honest unknown).
        for name in [
            "ground beef",
            "ground turkey",
            "ground pork",
            "ground lamb",
            "ground chicken",
            "ground almonds",
            "lean ground beef",
        ] {
            assert!(
                density_g_per_ml(name).is_none(),
                "{name} must not use spice density fallback"
            );
        }
        // Bare "… powder" / "… seeds" stay unknown unless listed.
        assert!(density_g_per_ml("mystery powder").is_none());
        assert!(density_g_per_ml("weird seeds").is_none());
    }

    #[test]
    fn unknown_returns_none() {
        assert!(density_g_per_ml("dragon scales").is_none());
    }
}
