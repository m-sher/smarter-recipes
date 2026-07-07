//! Embedded per-100 g macro table and per-item gram weights.
//!
//! Values are USDA-typical culinary estimates (raw for meat/produce, dry for
//! grains). Names not found here fall back to the cache/network source or are
//! reported as uncovered.

use crate::domain::{name_candidates, Macros};

const PER_100G: &[(&str, f64, f64, f64, f64)] = &[
    // fats & oils
    ("oil", 884.0, 0.0, 100.0, 0.0),
    ("olive oil", 884.0, 0.0, 100.0, 0.0),
    ("vegetable oil", 884.0, 0.0, 100.0, 0.0),
    ("canola oil", 884.0, 0.0, 100.0, 0.0),
    ("sesame oil", 884.0, 0.0, 100.0, 0.0),
    ("neutral oil", 884.0, 0.0, 100.0, 0.0),
    ("coconut oil", 862.0, 0.0, 100.0, 0.0),
    ("butter", 717.0, 0.9, 81.1, 0.1),
    ("cooking spray", 792.0, 0.0, 88.0, 0.0),
    // sweeteners
    ("sugar", 387.0, 0.0, 0.0, 100.0),
    ("granulated sugar", 387.0, 0.0, 0.0, 100.0),
    ("brown sugar", 380.0, 0.1, 0.0, 98.1),
    ("powdered sugar", 389.0, 0.0, 0.0, 99.8),
    ("honey", 304.0, 0.3, 0.0, 82.4),
    ("maple syrup", 260.0, 0.0, 0.1, 67.0),
    // flours, starches, grains (dry)
    ("flour", 364.0, 10.3, 1.0, 76.3),
    ("all-purpose flour", 364.0, 10.3, 1.0, 76.3),
    ("whole wheat flour", 340.0, 13.2, 2.5, 72.0),
    ("cornstarch", 381.0, 0.3, 0.1, 91.3),
    ("baking soda", 0.0, 0.0, 0.0, 0.0),
    ("baking powder", 53.0, 0.0, 0.0, 27.7),
    ("rice", 365.0, 7.1, 0.7, 80.0),
    ("white rice", 365.0, 7.1, 0.7, 80.0),
    ("brown rice", 370.0, 7.9, 2.9, 77.2),
    ("pasta", 371.0, 13.0, 1.5, 74.7),
    ("noodles", 384.0, 14.2, 4.4, 71.3),
    ("oats", 389.0, 16.9, 6.9, 66.3),
    ("bread", 265.0, 9.0, 3.2, 49.0),
    ("breadcrumbs", 395.0, 13.4, 5.3, 71.9),
    ("panko breadcrumbs", 375.0, 12.0, 2.5, 76.0),
    ("tortilla", 306.0, 8.2, 7.7, 50.4),
    // dairy & eggs
    ("milk", 61.0, 3.2, 3.3, 4.8),
    ("whole milk", 61.0, 3.2, 3.3, 4.8),
    ("buttermilk", 40.0, 3.3, 0.9, 4.8),
    ("heavy cream", 340.0, 2.8, 36.1, 2.8),
    ("sour cream", 198.0, 2.4, 19.4, 4.6),
    ("yogurt", 61.0, 3.5, 3.3, 4.7),
    ("greek yogurt", 97.0, 9.0, 5.0, 3.9),
    ("cheddar cheese", 403.0, 24.9, 33.1, 1.3),
    ("parmesan", 431.0, 38.5, 28.6, 4.1),
    ("mozzarella", 300.0, 22.2, 22.4, 2.2),
    ("cream cheese", 342.0, 5.9, 34.2, 4.1),
    ("feta", 264.0, 14.2, 21.3, 4.1),
    ("egg", 143.0, 12.6, 9.5, 0.7),
    ("eggs", 143.0, 12.6, 9.5, 0.7),
    ("egg yolk", 322.0, 15.9, 26.5, 3.6),
    ("egg white", 52.0, 10.9, 0.2, 0.7),
    // proteins (raw)
    ("chicken breast", 120.0, 22.5, 2.6, 0.0),
    ("chicken breasts", 120.0, 22.5, 2.6, 0.0),
    ("chicken thigh", 121.0, 19.7, 4.1, 0.0),
    ("chicken thighs", 121.0, 19.7, 4.1, 0.0),
    ("ground beef", 254.0, 17.2, 20.0, 0.0),
    ("beef", 250.0, 17.4, 20.0, 0.0),
    ("ground pork", 263.0, 16.9, 21.2, 0.0),
    ("pork", 242.0, 17.0, 19.0, 0.0),
    ("ground turkey", 148.0, 19.7, 7.7, 0.0),
    ("bacon", 417.0, 12.9, 39.7, 1.4),
    ("sausage", 346.0, 14.3, 31.3, 0.7),
    ("shrimp", 85.0, 20.1, 0.5, 0.0),
    ("salmon", 208.0, 20.4, 13.4, 0.0),
    ("tofu", 76.0, 8.1, 4.8, 1.9),
    // vegetables & fruit (raw)
    ("onion", 40.0, 1.1, 0.1, 9.3),
    ("onions", 40.0, 1.1, 0.1, 9.3),
    ("red onion", 40.0, 1.1, 0.1, 9.3),
    ("yellow onion", 40.0, 1.1, 0.1, 9.3),
    ("garlic", 149.0, 6.4, 0.5, 33.1),
    ("garlic clove", 149.0, 6.4, 0.5, 33.1),
    ("garlic cloves", 149.0, 6.4, 0.5, 33.1),
    ("ginger", 80.0, 1.8, 0.8, 17.8),
    ("tomato", 18.0, 0.9, 0.2, 3.9),
    ("tomatoes", 18.0, 0.9, 0.2, 3.9),
    ("cherry tomatoes", 18.0, 0.9, 0.2, 3.9),
    ("tomato sauce", 24.0, 1.2, 0.3, 5.3),
    ("tomato paste", 82.0, 4.3, 0.5, 18.9),
    ("potato", 77.0, 2.0, 0.1, 17.5),
    ("potatoes", 77.0, 2.0, 0.1, 17.5),
    ("sweet potato", 86.0, 1.6, 0.1, 20.1),
    ("carrot", 41.0, 0.9, 0.2, 9.6),
    ("carrots", 41.0, 0.9, 0.2, 9.6),
    ("celery", 16.0, 0.7, 0.2, 3.0),
    ("bell pepper", 26.0, 1.0, 0.3, 6.0),
    ("broccoli", 34.0, 2.8, 0.4, 6.6),
    ("spinach", 23.0, 2.9, 0.4, 3.6),
    ("mushroom", 22.0, 3.1, 0.3, 3.3),
    ("mushrooms", 22.0, 3.1, 0.3, 3.3),
    ("zucchini", 17.0, 1.2, 0.3, 3.1),
    ("cucumber", 15.0, 0.7, 0.1, 3.6),
    ("corn", 86.0, 3.3, 1.4, 19.0),
    ("peas", 81.0, 5.4, 0.4, 14.5),
    ("green beans", 31.0, 1.8, 0.2, 7.0),
    ("cabbage", 25.0, 1.3, 0.1, 5.8),
    ("cauliflower", 25.0, 1.9, 0.3, 5.0),
    ("avocado", 160.0, 2.0, 14.7, 8.5),
    ("jalapeno", 29.0, 0.9, 0.4, 6.5),
    ("shallot", 72.0, 2.5, 0.1, 16.8),
    ("green onion", 32.0, 1.8, 0.2, 7.3),
    ("green onions", 32.0, 1.8, 0.2, 7.3),
    ("scallion", 32.0, 1.8, 0.2, 7.3),
    ("scallions", 32.0, 1.8, 0.2, 7.3),
    ("cilantro", 23.0, 2.1, 0.5, 3.7),
    ("parsley", 36.0, 3.0, 0.8, 6.3),
    ("basil", 23.0, 3.2, 0.6, 2.7),
    ("lemon", 29.0, 1.1, 0.3, 9.3),
    ("lime", 30.0, 0.7, 0.2, 10.5),
    ("lemon juice", 22.0, 0.4, 0.2, 6.9),
    ("lime juice", 25.0, 0.4, 0.1, 8.4),
    ("banana", 89.0, 1.1, 0.3, 22.8),
    ("apple", 52.0, 0.3, 0.2, 13.8),
    // beans (canned, drained)
    ("black beans", 91.0, 6.0, 0.3, 16.6),
    ("chickpeas", 139.0, 7.0, 2.6, 22.5),
    // condiments & sauces
    ("soy sauce", 53.0, 8.1, 0.6, 4.9),
    ("light soy sauce", 53.0, 8.1, 0.6, 4.9),
    ("dark soy sauce", 60.0, 7.0, 0.3, 9.0),
    ("oyster sauce", 51.0, 1.4, 0.3, 11.0),
    ("fish sauce", 35.0, 5.1, 0.0, 3.6),
    ("hoisin sauce", 220.0, 3.3, 3.4, 44.1),
    ("worcestershire sauce", 78.0, 0.0, 0.0, 19.5),
    ("ketchup", 101.0, 1.0, 0.1, 27.4),
    ("mayonnaise", 680.0, 1.0, 74.9, 0.6),
    ("mustard", 60.0, 3.7, 3.3, 5.8),
    ("dijon mustard", 66.0, 4.4, 4.0, 5.8),
    ("vinegar", 18.0, 0.0, 0.0, 0.0),
    ("apple cider vinegar", 21.0, 0.0, 0.0, 0.9),
    ("rice vinegar", 18.0, 0.0, 0.0, 0.0),
    ("balsamic vinegar", 88.0, 0.5, 0.0, 17.0),
    ("hot sauce", 11.0, 0.5, 0.4, 1.8),
    ("sriracha", 93.0, 1.9, 0.9, 19.2),
    ("peanut butter", 588.0, 25.1, 50.4, 19.6),
    ("shaoxing wine", 88.0, 0.4, 0.0, 3.0),
    ("mirin", 226.0, 0.2, 0.0, 42.0),
    ("wine", 85.0, 0.1, 0.0, 2.6),
    ("chicken broth", 7.0, 0.6, 0.2, 0.5),
    ("chicken stock", 8.0, 1.0, 0.2, 0.4),
    ("beef broth", 7.0, 1.1, 0.2, 0.4),
    ("coconut milk", 230.0, 2.3, 23.8, 5.5),
    // baking & sweets
    ("vanilla extract", 288.0, 0.1, 0.1, 12.7),
    ("vanilla", 288.0, 0.1, 0.1, 12.7),
    ("cocoa powder", 228.0, 19.6, 13.7, 57.9),
    ("chocolate chips", 479.0, 4.2, 30.0, 63.9),
    ("yeast", 325.0, 40.4, 7.6, 41.2),
    // salt, water, spices & seasonings
    ("salt", 0.0, 0.0, 0.0, 0.0),
    ("salt and pepper", 0.0, 0.0, 0.0, 0.0),
    ("water", 0.0, 0.0, 0.0, 0.0),
    ("black pepper", 251.0, 10.4, 3.3, 63.9),
    ("pepper", 251.0, 10.4, 3.3, 63.9),
    ("white pepper", 296.0, 10.4, 2.1, 68.6),
    ("garlic powder", 331.0, 16.6, 0.7, 72.7),
    ("onion powder", 341.0, 10.4, 1.0, 79.1),
    ("chili powder", 282.0, 13.5, 14.3, 49.7),
    ("paprika", 282.0, 14.1, 12.9, 54.0),
    ("smoked paprika", 282.0, 14.1, 12.9, 54.0),
    ("cumin", 375.0, 17.8, 22.3, 44.2),
    ("cinnamon", 247.0, 4.0, 1.2, 80.6),
    ("red pepper flakes", 318.0, 12.0, 17.3, 56.6),
    ("cayenne pepper", 318.0, 12.0, 17.3, 56.6),
    ("italian seasoning", 265.0, 9.0, 4.3, 65.0),
    ("oregano", 265.0, 9.0, 4.3, 68.9),
    ("thyme", 276.0, 9.1, 7.4, 63.9),
    ("sesame seeds", 573.0, 17.7, 49.7, 23.4),
];

/// Culinary average grams per discrete piece (count unit). Longer / more specific
/// keys should appear before shorter aliases that `name_candidates` may try.
const PER_EACH_G: &[(&str, f64)] = &[
    ("egg", 50.0),
    ("eggs", 50.0),
    ("egg yolk", 17.0),
    ("egg white", 33.0),
    // Garlic — must stay before bare "cloves" (spice) for candidate fallback order.
    ("garlic", 3.0),
    ("garlic clove", 3.0),
    ("garlic cloves", 3.0),
    ("onion", 110.0),
    ("onions", 110.0),
    ("red onion", 110.0),
    ("yellow onion", 110.0),
    ("shallot", 25.0),
    ("chicken breast", 174.0),
    ("chicken breasts", 174.0),
    ("chicken thigh", 116.0),
    ("chicken thighs", 116.0),
    ("lemon", 58.0),
    ("lime", 44.0),
    ("tomato", 123.0),
    ("tomatoes", 123.0),
    ("potato", 213.0),
    ("potatoes", 213.0),
    ("russet", 213.0),
    ("russet potato", 213.0),
    ("russet potatoes", 213.0),
    ("sweet potato", 130.0),
    ("carrot", 61.0),
    ("carrots", 61.0),
    ("celery", 40.0),
    ("bell pepper", 119.0),
    ("scallion", 15.0),
    ("scallions", 15.0),
    ("green onion", 15.0),
    ("green onions", 15.0),
    ("jalapeno", 14.0),
    ("jalapeño", 14.0),
    // Fresh chiles (serrano-class default ~15 g; dried pods much lighter).
    ("green chile pepper", 15.0),
    ("green chile peppers", 15.0),
    ("green chili pepper", 15.0),
    ("green chili peppers", 15.0),
    ("green chilli", 15.0),
    ("green chillies", 15.0),
    ("green chilies", 15.0),
    ("fresh green chile pepper", 15.0),
    ("serrano", 15.0),
    ("serrano pepper", 15.0),
    ("serrano peppers", 15.0),
    ("dried red chile pepper", 2.0),
    ("dried red chile peppers", 2.0),
    ("dried red chili", 2.0),
    ("dried red chilies", 2.0),
    ("dried red chilli", 2.0),
    ("dried red chillies", 2.0),
    ("chile de arbol", 2.0),
    ("red chile pepper", 15.0),
    ("red chile peppers", 15.0),
    // Whole spices / aromatics (counts).
    ("black cardamom pod", 1.0),
    ("black cardamom pods", 1.0),
    ("green cardamom pod", 0.3),
    ("green cardamom pods", 0.3),
    ("cardamom pod", 0.3),
    ("cardamom pods", 0.3),
    ("green cardamom seeds", 0.05),
    ("bay leaf", 0.4),
    ("bay leaves", 0.4),
    ("curry leaf", 0.15),
    ("curry leaves", 0.15),
    // Whole spice cloves only (not bare "cloves" — that is common garlic shorthand
    // and would under-weigh badly if mapped to ~0.1 g spice cloves).
    ("whole cloves", 0.1),
    ("stick cinnamon", 3.0),
    ("sticks cinnamon", 3.0),
    ("cinnamon stick", 3.0),
    ("cinnamon sticks", 3.0),
    ("star anise", 0.5),
    ("black peppercorn", 0.05),
    ("black peppercorns", 0.05),
    ("peppercorn", 0.05),
    ("peppercorns", 0.05),
    // Ginger pieces / slices (count of slices or chunks).
    ("quarter-size slices peeled fresh ginger", 5.0),
    ("slice peeled fresh ginger", 5.0),
    ("slices peeled fresh ginger", 5.0),
    ("cm/1in root ginger", 8.0),
    ("inch ginger root", 8.0),
    ("inch fresh ginger", 8.0),
    ("avocado", 136.0),
    ("banana", 118.0),
    ("apple", 182.0),
    ("bread", 29.0),
    ("tortilla", 30.0),
    ("cucumber", 201.0),
    ("zucchini", 196.0),
    ("bacon", 28.0),
    ("mushroom", 18.0),
    ("mushrooms", 18.0),
    ("cooking spray", 0.3),
];

/// Per-100 g macros for an ingredient name (candidate matching: full name,
/// then last token, then last hyphen segment).
pub fn per_100g(name: &str) -> Option<Macros> {
    name_candidates(name).iter().find_map(|c| per_100g_exact(c))
}

/// Per-100 g macros for an exact table key (no candidate expansion).
pub fn per_100g_exact(name: &str) -> Option<Macros> {
    PER_100G
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|&(_, kcal, protein_g, fat_g, carbs_g)| Macros {
            kcal,
            protein_g,
            fat_g,
            carbs_g,
        })
}

/// Grams for one item of a count-kind ingredient.
pub fn grams_per_each(name: &str) -> Option<f64> {
    for cand in name_candidates(name) {
        if let Some(&(_, g)) = PER_EACH_G.iter().find(|(n, _)| *n == cand) {
            return Some(g);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classics_present_and_plausible() {
        let butter = per_100g("butter").unwrap();
        assert!((butter.kcal - 717.0).abs() < 1.0);
        let oil = per_100g("olive oil").unwrap();
        assert!((oil.fat_g - 100.0).abs() < 0.1);
        let sugar = per_100g("sugar").unwrap();
        assert!((sugar.carbs_g - 100.0).abs() < 1.0);
        assert_eq!(per_100g("salt").unwrap().kcal, 0.0);
    }

    #[test]
    fn candidate_matching_reaches_generic_entries() {
        assert!(per_100g("all-purpose flour").is_some());
        assert!(per_100g("bread flour").is_some()); // falls back to "flour"
        assert!(per_100g("minced garlic").is_some());
        assert!(per_100g("dragon scales").is_none());
    }

    #[test]
    fn per_each_weights() {
        assert_eq!(grams_per_each("eggs"), Some(50.0));
        assert_eq!(grams_per_each("large eggs"), Some(50.0));
        assert!(grams_per_each("mystery item").is_none());
    }

    #[test]
    fn spice_and_chile_each_weights() {
        assert_eq!(grams_per_each("green chile peppers"), Some(15.0));
        assert_eq!(grams_per_each("green chillies"), Some(15.0));
        assert_eq!(grams_per_each("dried red chile peppers"), Some(2.0));
        assert_eq!(grams_per_each("bay leaves"), Some(0.4));
        assert_eq!(grams_per_each("green cardamom pods"), Some(0.3));
        assert_eq!(grams_per_each("black cardamom pods"), Some(1.0));
        // Garlic must not pick up a spice-clove weight; bare "cloves" stays unknown
        // (ambiguous garlic shorthand vs whole spice).
        assert_eq!(grams_per_each("garlic cloves"), Some(3.0));
        assert_eq!(grams_per_each("whole cloves"), Some(0.1));
        assert!(grams_per_each("cloves").is_none());
        assert_eq!(grams_per_each("stick cinnamon"), Some(3.0));
    }

    #[test]
    fn no_duplicate_names_within_tables() {
        let mut seen = std::collections::HashSet::new();
        for (n, ..) in PER_100G {
            assert!(seen.insert(*n), "duplicate PER_100G entry: {n}");
        }
        let mut seen = std::collections::HashSet::new();
        for (n, _) in PER_EACH_G {
            assert!(seen.insert(*n), "duplicate PER_EACH_G entry: {n}");
        }
    }
}
