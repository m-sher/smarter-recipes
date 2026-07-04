//! Structural "is this actually a cookable recipe?" check, used to keep
//! non-meals — roundups/listicles, index pages, how-to guides, and extraction
//! failures — out of the catalog. Deliberately keyword-free: it reads the
//! recipe's structure, never its title text.

use crate::domain::Recipe;

/// Unicode vulgar fractions the ingest normalizer understands (mirrors
/// `normalize::parse`), so a line like "½ cup sugar" counts as carrying an amount.
const VULGAR_FRACTIONS: &[char] = &[
    '½', '¼', '¾', '⅓', '⅔', '⅕', '⅖', '⅗', '⅘', '⅙', '⅚', '⅛', '⅜', '⅝', '⅞',
];

/// Whether an ingredient line carries an **amount**: a parsed quantity, or a
/// digit / vulgar fraction anywhere in the raw text (which catches amounts the
/// leading-quantity parser misses, e.g. "cauliflower florets (12 ounces)").
fn line_has_amount(line: &crate::domain::IngredientLine) -> bool {
    line.quantity.is_some()
        || line
            .original
            .chars()
            .any(|c| c.is_ascii_digit() || VULGAR_FRACTIONS.contains(&c))
}

/// True when a recipe looks like a real, cookable dish rather than a roundup,
/// index page, how-to guide, or extraction failure.
///
/// The load-bearing signal is how many ingredient lines actually carry an
/// amount. Real recipes quantify their ingredients; roundups and index pages
/// list dish *titles* as "ingredients" and carry almost none. The `>= 2` floor
/// keeps valid two-ingredient condiments (miso butter, chili crisp mayo); the
/// `>= 0.25 * ingredients` ratio rejects amount-sparse lists regardless of size.
pub fn is_cookable(recipe: &Recipe) -> bool {
    let ing = recipe.ingredients.len();
    let amt = recipe
        .ingredients
        .iter()
        .filter(|l| line_has_amount(l))
        .count();
    amt >= 2 && (amt as f64) >= 0.25 * (ing as f64)
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
    fn keeps_real_recipes_and_two_ingredient_condiments() {
        assert!(is_cookable(&rec(&[
            "2 cups flour",
            "1 egg",
            "1/2 cup sugar",
            "salt to taste"
        ])));
        // A valid condiment: only two ingredients, both with amounts.
        assert!(is_cookable(&rec(&["2 tbsp white miso", "3 tbsp butter"])));
        // Amount only expressed as a unicode fraction.
        assert!(is_cookable(&rec(&[
            "½ cup soy sauce",
            "¼ cup rice vinegar"
        ])));
    }

    #[test]
    fn rejects_roundups_guides_and_failures() {
        // Roundup: 16 dish titles, only 3 carrying a number → 3 < 0.25*16.
        let mut ings = vec!["Creamy Tuscan Chicken"; 13];
        ings.extend(["30 Minute Chicken", "5 Bean Chili", "1 Pot Pasta"]);
        assert!(!is_cookable(&rec(&ings)));
        // How-to / single-ingredient prep.
        assert!(!is_cookable(&rec(&["1 large onion"])));
        // Extraction failure with no ingredients.
        assert!(!is_cookable(&rec(&[])));
    }
}
