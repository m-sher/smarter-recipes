//! Structural "is this a cookable recipe?" check that keeps non-meals —
//! roundups/listicles, index pages, how-to guides, and extraction failures —
//! out of the catalog. Keyword-free: reads the recipe's structure, never its
//! title text.

use crate::domain::Recipe;
use once_cell::sync::Lazy;
use regex::Regex;

/// Unicode vulgar fractions the ingest normalizer understands.
const VULGAR_FRACTIONS: &[char] = &[
    '½', '¼', '¾', '⅓', '⅔', '⅕', '⅖', '⅗', '⅘', '⅙', '⅚', '⅛', '⅜', '⅝', '⅞',
];

/// A standalone numeric amount: a digit run (optionally with `.`, `,`, `/`)
/// bounded by a non-alphanumeric edge — matches "8 scallops", "1/2 cup",
/// "(12 ounces)", "serves 4"; rejects title-embedded digits like "30-Minute",
/// "2% milk", "7-Up", "5-Ingredient".
static RE_STANDALONE_NUMBER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:^|[\s(\[])\d[\d.,/]*(?:[\s)\],]|$)").expect("number re"));
/// A number glued to a common unit abbreviation, e.g. "500g", "2tbsp".
static RE_GLUED_UNIT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b\d[\d.,/]*(?:g|kg|mg|ml|oz|lb|tsp|tbsp)\b").expect("glued re"));

/// Whether raw text carries a real numeric amount (same rule as cookable checks).
///
/// Used by EPUB headnote stripping: drop leading non-amount prose until the first
/// amount-bearing line, then keep the rest as candidate ingredients.
pub fn text_has_amount(text: &str) -> bool {
    text.chars().any(|c| VULGAR_FRACTIONS.contains(&c))
        || RE_STANDALONE_NUMBER.is_match(text)
        || RE_GLUED_UNIT.is_match(text)
}

/// Whether an ingredient line carries a real numeric amount. Reads the raw text,
/// not the parser's `quantity`.
fn line_has_amount(line: &crate::domain::IngredientLine) -> bool {
    text_has_amount(&line.original)
}

/// Upper bound on ingredient lines for a single dish. Larger lists are almost
/// always a chapter/index mis-segmentation (hundreds of lines), not one recipe.
/// Set high enough for elaborate multi-component dishes (biryani, thali-style
/// feasts, spice-heavy curries) while still rejecting mis-segmented chapters.
pub const MAX_COOKABLE_INGREDIENTS: usize = 100;

/// True when a recipe looks like a real, cookable dish rather than a roundup,
/// index page, how-to guide, or extraction failure.
///
/// The rule: at least two ingredient lines, at least one carrying an amount,
/// amounts at least a fifth of the lines, and not an implausibly huge list.
pub fn is_cookable(recipe: &Recipe) -> bool {
    let ing = recipe.ingredients.len();
    if !(2..=MAX_COOKABLE_INGREDIENTS).contains(&ing) {
        return false;
    }
    let amt = recipe
        .ingredients
        .iter()
        .filter(|l| line_has_amount(l))
        .count();
    amt >= 1 && (amt as f64) >= 0.2 * (ing as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::normalize_line;

    fn rec(ings: &[&str]) -> Recipe {
        let mut r = Recipe::new("t");
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    #[test]
    fn keeps_real_recipes_including_sparse_and_condiments() {
        assert!(is_cookable(&rec(&[
            "2 cups flour",
            "1 egg",
            "1/2 cup sugar",
            "salt to taste"
        ])));
        // Two-ingredient condiment.
        assert!(is_cookable(&rec(&["2 tbsp white miso", "3 tbsp butter"])));
        // Amount only as a unicode fraction.
        assert!(is_cookable(&rec(&[
            "½ cup soy sauce",
            "¼ cup rice vinegar"
        ])));
        // Minimalist real recipes: one quantified ingredient + unquantified extras.
        assert!(is_cookable(&rec(&[
            "1½ cups cooked chickpeas",
            "extra-virgin olive oil",
            "sea salt",
            "paprika or other spices"
        ]))); // Crispy Roasted Chickpeas
        assert!(is_cookable(&rec(&[
            "8 scallops",
            "salt to taste",
            "cracked black pepper",
            "cooking spray",
            "lemon wedges"
        ]))); // Air Fryer Scallops
    }

    #[test]
    fn rejects_roundups_guides_and_failures() {
        // Roundup: dish titles as "ingredients", no amounts.
        assert!(!is_cookable(&rec(&[
            "Fluffy Quinoa",
            "Cherry Pistachio Quinoa Salad",
            "Kale and Quinoa Salad",
            "Quinoa Stuffed Peppers",
            "Black Bean and Quinoa Bake",
        ])));
        // How-to / single-ingredient prep.
        assert!(!is_cookable(&rec(&["1 large onion"])));
        // Extraction failure with no ingredients.
        assert!(!is_cookable(&rec(&[])));
    }

    #[test]
    fn rejects_implausibly_huge_ingredient_lists() {
        let lines: Vec<String> = (0..MAX_COOKABLE_INGREDIENTS + 1)
            .map(|i| format!("{} g spice{i}", i + 1))
            .collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        assert!(!is_cookable(&rec(&refs)));
        // Elaborate multi-component dishes (well above the old 48 cap) stay cookable.
        let ok_lines: Vec<String> = (0..60).map(|i| format!("{} g spice{i}", i + 1)).collect();
        let ok_refs: Vec<&str> = ok_lines.iter().map(|s| s.as_str()).collect();
        assert!(is_cookable(&rec(&ok_refs)));
        let at_cap: Vec<String> = (0..MAX_COOKABLE_INGREDIENTS)
            .map(|i| format!("{} g spice{i}", i + 1))
            .collect();
        let at_cap_refs: Vec<&str> = at_cap.iter().map(|s| s.as_str()).collect();
        assert!(is_cookable(&rec(&at_cap_refs)));
    }

    #[test]
    fn title_embedded_digits_do_not_count_as_amounts() {
        // Digits inside names/titles must NOT register as amounts.
        for line in [
            "30-Minute Garlic Chicken",
            "2% milk",
            "7-Up",
            "5-Ingredient Pasta",
        ] {
            assert!(
                !line_has_amount(&normalize_line(line)),
                "{line} wrongly counted as an amount"
            );
        }
        // A roundup whose titles all carry embedded (non-amount) numbers is rejected.
        assert!(!is_cookable(&rec(&[
            "30-Minute Chicken",
            "20-Minute Shrimp",
            "15-Minute Pasta",
            "5-Ingredient Chili",
        ])));
    }
}
