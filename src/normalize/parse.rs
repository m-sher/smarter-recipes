//! Parse free-text ingredient lines into name, quantity, and unit.
//!
//! Supported patterns (representative, not exhaustive):
//! - `2 cups flour`
//! - `1/2 tsp salt`
//! - `1 1/2 cups milk` (mixed numbers)
//! - `2.5 lb chicken breast, diced`
//! - `salt to taste` (no quantity)
//! - `a pinch of paprika`
//! - `one onion`
//! - `500g pasta` (unit glued to number)
//! - `2-3 cloves garlic` (range → midpoint, marked uncertain)

use crate::domain::{ParsedIngredient, Unit};
use once_cell::sync::Lazy;
use regex::Regex;

use super::units::lookup_unit;

static RE_LEADING_QTY: Lazy<Regex> = Lazy::new(|| {
    // quantity: optional range, mixed number, fraction, or decimal
    Regex::new(
        r"(?x)
        ^\s*
        (?:
            (?P<range_lo>\d+(?:\.\d+)?)\s*[-–—to]+\s*(?P<range_hi>\d+(?:\.\d+)?)  # 2-3 or 2 to 3
          | (?P<mixed_w>\d+)\s+(?P<mixed_n>\d+)\s*/\s*(?P<mixed_d>\d+)            # 1 1/2
          | (?P<frac_n>\d+)\s*/\s*(?P<frac_d>\d+)                                 # 1/2
          | (?P<dec>\d+(?:\.\d+)?)                                                # 2 or 2.5
          | (?P<word>a|an|one|two|three|four|five|six|seven|eight|nine|ten|half|quarter)\b
        )
        \s*
        ",
    )
    .expect("qty regex")
});

static RE_GLUED_UNIT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?P<num>\d+(?:\.\d+)?)(?P<unit>g|kg|mg|ml|l|oz|lb|lbs|tsp|tbsp)$")
        .expect("glued")
});

static RE_UNIT_THEN_NAME: Lazy<Regex> = Lazy::new(|| {
    // unit token (possibly multi-word like "fl oz") then the rest
    Regex::new(
        r"(?ix)
        ^\s*
        (?P<unit>
            fl\.?\s*oz\.?
          | fluid\s+ounces?
          | tablespoons? | tbsps? | tbs | tbl
          | teaspoons? | tsps?
          | cups? | ounces? | pounds? | lbs?
          | grams? | kilograms? | milliliters? | millilitres? | liters? | litres?
          | cloves? | slices? | pieces? | pcs? | cans? | bunches? | heads?
          | stalks? | sprigs? | leaves? | packets? | packages? | bags?
          | jars? | bottles? | pinches? | dashes? | whole | each | ea
          | g | kg | mg | ml | l | oz | lb | tsp | tbsp | c | t | pt | qt | gal
        )
        \b
        \s*
        (?:of\s+)?
        (?P<rest>.+)
        $
        ",
    )
    .expect("unit name")
});

static RE_LEADING_COUNT_PAREN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(?P<count>\d+(?:\.\d+)?)?\s*\((?P<inner>[^)]*)\)\s*(?P<rest>.+)$")
        .expect("paren size regex")
});

/// Drop a leading container noun ("can", "jars", …) from a name fragment.
fn strip_container_word(rest: &str) -> &str {
    const CONTAINERS: &[&str] = &[
        "cans",
        "can",
        "jars",
        "jar",
        "bottles",
        "bottle",
        "packages",
        "package",
        "packets",
        "packet",
        "bags",
        "bag",
        "boxes",
        "box",
        "tins",
        "tin",
        "containers",
        "container",
        "cartons",
        "carton",
    ];
    let mut parts = rest.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    if CONTAINERS.contains(&first.to_lowercase().as_str()) {
        parts.next().unwrap_or("").trim()
    } else {
        rest
    }
}

/// Parse "<count> (<qty> <unit>) [container] <name>", e.g. "2 (400g) cans tomatoes".
/// Quantity is `count * size`. Returns `None` when the shape doesn't match or the
/// parenthetical is not a quantity+unit.
fn try_parenthetical_size(line: &str) -> Option<ParsedIngredient> {
    let caps = RE_LEADING_COUNT_PAREN.captures(line)?;
    let inner = caps.name("inner")?.as_str().trim();
    let rest = caps.name("rest")?.as_str().trim();

    let (size_qty, _, end) = parse_quantity_prefix(inner)?;
    let unit_tok = inner[end..].split_whitespace().next().unwrap_or("");
    let unit = lookup_unit(unit_tok)?;

    let count: f64 = caps
        .name("count")
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(1.0);

    let (name, note) = split_note(strip_container_word(rest));
    let name = clean_name(&name);
    if name.is_empty() {
        return None;
    }

    Some(ParsedIngredient {
        name,
        quantity: Some(count * size_qty),
        unit: Some(unit),
        note,
        uncertain: false,
    })
}

/// Rewrite Unicode vulgar fractions to ASCII.
/// `1½` and `1 ½` become `1 1/2`; a standalone `¼` becomes `1/4`.
fn rewrite_unicode_fractions(s: &str) -> String {
    fn expand(c: char) -> Option<&'static str> {
        Some(match c {
            '½' => "1/2",
            '¼' => "1/4",
            '¾' => "3/4",
            '⅓' => "1/3",
            '⅔' => "2/3",
            '⅕' => "1/5",
            '⅖' => "2/5",
            '⅗' => "3/5",
            '⅘' => "4/5",
            '⅙' => "1/6",
            '⅚' => "5/6",
            '⅛' => "1/8",
            '⅜' => "3/8",
            '⅝' => "5/8",
            '⅞' => "7/8",
            _ => return None,
        })
    }
    if !s.chars().any(|c| expand(c).is_some()) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match expand(c) {
            Some(frac) => {
                if out
                    .trim_end()
                    .chars()
                    .last()
                    .is_some_and(|p| p.is_ascii_digit())
                {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push(' ');
                }
                out.push_str(frac);
            }
            None => out.push(c),
        }
    }
    out
}

fn word_quantity(w: &str) -> Option<f64> {
    match w.to_lowercase().as_str() {
        "a" | "an" | "one" => Some(1.0),
        "two" => Some(2.0),
        "three" => Some(3.0),
        "four" => Some(4.0),
        "five" => Some(5.0),
        "six" => Some(6.0),
        "seven" => Some(7.0),
        "eight" => Some(8.0),
        "nine" => Some(9.0),
        "ten" => Some(10.0),
        "half" => Some(0.5),
        "quarter" => Some(0.25),
        _ => None,
    }
}

fn parse_quantity_prefix(s: &str) -> Option<(f64, bool, usize)> {
    // Returns (qty, uncertain, bytes_consumed)
    let caps = RE_LEADING_QTY.captures(s)?;
    let uncertain_range = caps.name("range_lo").is_some();
    let qty = if let (Some(lo), Some(hi)) = (caps.name("range_lo"), caps.name("range_hi")) {
        let a: f64 = lo.as_str().parse().ok()?;
        let b: f64 = hi.as_str().parse().ok()?;
        (a + b) / 2.0
    } else if let (Some(w), Some(n), Some(d)) = (
        caps.name("mixed_w"),
        caps.name("mixed_n"),
        caps.name("mixed_d"),
    ) {
        let whole: f64 = w.as_str().parse().ok()?;
        let num: f64 = n.as_str().parse().ok()?;
        let den: f64 = d.as_str().parse().ok()?;
        if den == 0.0 {
            return None;
        }
        whole + num / den
    } else if let (Some(n), Some(d)) = (caps.name("frac_n"), caps.name("frac_d")) {
        let num: f64 = n.as_str().parse().ok()?;
        let den: f64 = d.as_str().parse().ok()?;
        if den == 0.0 {
            return None;
        }
        num / den
    } else if let Some(dec) = caps.name("dec") {
        dec.as_str().parse().ok()?
    } else if let Some(word) = caps.name("word") {
        word_quantity(word.as_str())?
    } else {
        return None;
    };
    let end = caps.get(0)?.end();
    Some((qty, uncertain_range, end))
}

/// Split trailing notes after comma or parentheticals.
fn split_note(rest: &str) -> (String, Option<String>) {
    let rest = rest.trim();
    // Parenthetical note
    if let Some(open) = rest.find('(') {
        if let Some(close) = rest.rfind(')') {
            if close > open {
                let name = rest[..open].trim().trim_end_matches(',').trim().to_string();
                let note = rest[open + 1..close].trim().to_string();
                if !name.is_empty() {
                    return (name, Some(note));
                }
            }
        }
    }
    // Comma-separated note
    if let Some(idx) = rest.find(',') {
        let before = rest[..idx].trim();
        let after = rest[idx + 1..].trim();
        if !before.is_empty() && !after.is_empty() {
            // A leading run of descriptors isn't the name; scan the remainder.
            if crate::domain::is_all_descriptors(before) {
                return split_note(after);
            }
            return (before.to_string(), Some(after.to_string()));
        }
    }
    (rest.to_string(), None)
}

/// Earliest char-boundary byte offset in `s` where any phrase begins
/// (case-insensitive).
fn taste_phrase_start(s: &str, phrases: &[&str]) -> Option<usize> {
    s.char_indices().find_map(|(byte_idx, _)| {
        let rest = s[byte_idx..].to_lowercase();
        phrases
            .iter()
            .any(|p| rest.starts_with(p))
            .then_some(byte_idx)
    })
}

/// Remove every parenthetical group `(…)` and collapse the whitespace it leaves.
/// Depth-counted, handling nested and unbalanced parens.
fn strip_parentheticals(s: &str) -> String {
    if !s.contains(['(', ')']) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip a leading `+ <qty> <unit>` addend, e.g. the `+ 3 tablespoons` in
/// `1 cup + 3 tablespoons beef broth`.
fn strip_leading_addend(s: &str) -> String {
    let Some(rest) = s.trim().strip_prefix('+') else {
        return s.to_string();
    };
    let rest = rest.trim();
    if let Some((_, _, end)) = parse_quantity_prefix(rest) {
        let after_qty = rest[end..].trim();
        let mut parts = after_qty.splitn(2, char::is_whitespace);
        let first = parts.next().unwrap_or("");
        if lookup_unit(first).is_some() {
            return parts.next().unwrap_or("").trim().to_string();
        }
        return after_qty.to_string();
    }
    rest.to_string()
}

fn clean_name(name: &str) -> String {
    let without_parens = strip_parentheticals(name);
    let addend_stripped = strip_leading_addend(&without_parens);
    // Drop leading non-alphanumeric characters, keeping digits.
    let mut s = addend_stripped
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string();
    // Strip leading "of "
    if let Some(stripped) = s.strip_prefix("of ") {
        s = stripped.trim().to_string();
    }
    // Remove trailing punctuation
    while s.ends_with(['.', ';']) {
        s.pop();
    }
    s
}

/// Parse a single free-text ingredient line.
pub fn parse_ingredient_line(line: &str) -> ParsedIngredient {
    // Decode HTML entities and fold curly punctuation.
    let sanitized = crate::text::sanitize(line);
    let line = sanitized.trim();
    if line.is_empty() {
        return ParsedIngredient {
            name: String::new(),
            quantity: None,
            unit: None,
            note: None,
            uncertain: true,
        };
    }

    let rewritten = rewrite_unicode_fractions(line);
    let line = rewritten.as_str();

    // Handle "to taste" / "as needed" only when there is no leading quantity;
    // with a quantity, parse normally and attach the note.
    let lower = line.to_lowercase();
    let has_taste_note =
        lower.contains("to taste") || lower.contains("as needed") || lower.contains("as desired");
    if has_taste_note && parse_quantity_prefix(line).is_none() {
        // Strip trailing taste notes from the name, preserving original casing.
        let mut name = line.to_string();
        if let Some(cut) = taste_phrase_start(
            &name,
            &["or to taste", "to taste", "as needed", "as desired"],
        ) {
            name.truncate(cut);
        }
        // Also drop a dangling "(" from a parenthesized note, e.g.
        // "salt (to taste)" -> "salt".
        name = name.trim().trim_end_matches([',', '(']).trim().to_string();
        let name = if name.is_empty() {
            clean_name(line)
        } else {
            clean_name(&name)
        };
        return ParsedIngredient {
            name,
            quantity: None,
            unit: None,
            note: Some("to taste / as needed".into()),
            uncertain: true,
        };
    }

    // Leading count + parenthetical package size, e.g. "1 (14 oz) can tomatoes".
    if let Some(parsed) = try_parenthetical_size(line) {
        return parsed;
    }

    // Glued number+unit at start: "500g pasta"
    if let Some(caps) = RE_GLUED_UNIT.captures(line.split_whitespace().next().unwrap_or("")) {
        let num: f64 = caps["num"].parse().unwrap_or(0.0);
        let unit_str = &caps["unit"];
        let unit = lookup_unit(unit_str);
        let uncertain = unit.is_none();
        let rest = line[caps.get(0).unwrap().end()..].trim();
        let (name, note) = split_note(rest);
        return ParsedIngredient {
            name: clean_name(&name),
            quantity: Some(num),
            unit,
            note,
            uncertain,
        };
    }

    let mut uncertain = false;
    let (qty, rest_after_qty) = if let Some((q, u, end)) = parse_quantity_prefix(line) {
        uncertain |= u;
        (Some(q), &line[end..])
    } else {
        (None, line)
    };

    let rest_after_qty = rest_after_qty.trim();

    // Try unit then name
    let (unit, name_part, unit_uncertain) =
        if let Some(caps) = RE_UNIT_THEN_NAME.captures(rest_after_qty) {
            let unit_str = caps.name("unit").unwrap().as_str();
            let rest = caps.name("rest").unwrap().as_str();
            let u = lookup_unit(unit_str);
            (u.clone(), rest, u.is_none())
        } else if qty.is_some() {
            // Quantity but no recognized unit — first token might be an unknown unit
            let mut parts = rest_after_qty.splitn(2, char::is_whitespace);
            let first = parts.next().unwrap_or("");
            let rem = parts.next().unwrap_or("").trim();
            if !first.is_empty() && !rem.is_empty() {
                if let Some(u) = lookup_unit(first) {
                    (Some(u), rem, false)
                } else {
                    // Assume count (e.g. "2 eggs") — name is full rest
                    (None, rest_after_qty, false)
                }
            } else {
                (None, rest_after_qty, false)
            }
        } else {
            (None, rest_after_qty, false)
        };
    uncertain |= unit_uncertain;

    // "a pinch of X" — word qty + unit
    let (qty, unit) = match (qty, unit) {
        (None, None) => {
            // Try "pinch of X" / "dash of X"
            if let Some(caps) = RE_UNIT_THEN_NAME.captures(line) {
                let unit_str = caps.name("unit").unwrap().as_str();
                if matches!(
                    unit_str.to_lowercase().as_str(),
                    "pinch" | "pinches" | "dash" | "dashes"
                ) {
                    let rest = caps.name("rest").unwrap().as_str();
                    let (name, note) = split_note(rest);
                    return ParsedIngredient {
                        name: clean_name(&name),
                        quantity: Some(1.0),
                        unit: lookup_unit(unit_str),
                        note,
                        uncertain: false,
                    };
                }
            }
            (None, None)
        }
        (q, u) => (q, u),
    };

    let (name, note) = split_note(name_part);
    let name = clean_name(&name);

    let unit: Option<Unit> = unit;

    let uncertain = uncertain || (qty.is_some() && unit.is_none() && name.is_empty());

    let note = if has_taste_note {
        match note {
            Some(n) => Some(format!("{n}; to taste / as needed")),
            None => Some("to taste / as needed".into()),
        }
    } else {
        note
    };

    ParsedIngredient {
        name: if name.is_empty() {
            line.to_string()
        } else {
            name
        },
        quantity: qty,
        unit,
        note,
        uncertain,
    }
}

/// Parse many lines, skipping blanks.
pub fn parse_ingredient_lines<'a, I>(lines: I) -> Vec<ParsedIngredient>
where
    I: IntoIterator<Item = &'a str>,
{
    lines
        .into_iter()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(parse_ingredient_line)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::UnitKind;
    use pretty_assertions::assert_eq;

    fn qty_unit(line: &str) -> (Option<f64>, Option<UnitKind>, String) {
        let p = parse_ingredient_line(line);
        (p.quantity, p.unit.as_ref().map(|u| u.kind), p.name)
    }

    #[test]
    fn simple_cups() {
        let (q, k, n) = qty_unit("2 cups flour");
        assert_eq!(q, Some(2.0));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "flour");
    }

    #[test]
    fn fraction() {
        let (q, k, n) = qty_unit("1/2 tsp salt");
        assert_eq!(q, Some(0.5));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "salt");
    }

    #[test]
    fn mixed_number() {
        let (q, k, n) = qty_unit("1 1/2 cups milk");
        assert_eq!(q, Some(1.5));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "milk");
    }

    #[test]
    fn decimal_mass() {
        let (q, k, n) = qty_unit("2.5 lb chicken breast, diced");
        assert_eq!(q, Some(2.5));
        assert_eq!(k, Some(UnitKind::Mass));
        assert_eq!(n, "chicken breast");
        let p = parse_ingredient_line("2.5 lb chicken breast, diced");
        assert_eq!(p.note.as_deref(), Some("diced"));
    }

    #[test]
    fn glued_unit() {
        let (q, k, n) = qty_unit("500g pasta");
        assert_eq!(q, Some(500.0));
        assert_eq!(k, Some(UnitKind::Mass));
        assert_eq!(n, "pasta");
    }

    #[test]
    fn range_midpoint_uncertain() {
        let p = parse_ingredient_line("2-3 cloves garlic");
        assert_eq!(p.quantity, Some(2.5));
        assert!(p.uncertain);
        assert_eq!(p.name, "garlic");
        assert_eq!(p.unit.as_ref().map(|u| u.kind), Some(UnitKind::Count));
    }

    #[test]
    fn word_quantity() {
        let (q, _, n) = qty_unit("one onion");
        assert_eq!(q, Some(1.0));
        assert_eq!(n, "onion");
    }

    #[test]
    fn to_taste() {
        let p = parse_ingredient_line("salt to taste");
        assert!(p.quantity.is_none());
        assert!(p.uncertain);
        assert!(p.name.to_lowercase().contains("salt"));
    }

    #[test]
    fn pinch_of() {
        let p = parse_ingredient_line("a pinch of paprika");
        assert_eq!(p.quantity, Some(1.0));
        assert_eq!(p.unit.as_ref().map(|u| u.kind), Some(UnitKind::Volume));
        assert_eq!(p.name, "paprika");
    }

    #[test]
    fn parenthetical_note() {
        let p = parse_ingredient_line("2 eggs (large)");
        assert_eq!(p.quantity, Some(2.0));
        assert_eq!(p.name, "eggs");
        assert_eq!(p.note.as_deref(), Some("large"));
    }

    #[test]
    fn tablespoons_abbrev() {
        let (q, k, n) = qty_unit("3 tbsp olive oil");
        assert_eq!(q, Some(3.0));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "olive oil");
    }

    #[test]
    fn of_connector() {
        let (q, k, n) = qty_unit("2 cups of sugar");
        assert_eq!(q, Some(2.0));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "sugar");
    }

    #[test]
    fn count_items() {
        let (q, k, n) = qty_unit("4 eggs");
        assert_eq!(q, Some(4.0));
        // No unit token → count by implication (unit None is ok; aggregation uses Count)
        assert!(k.is_none() || k == Some(UnitKind::Count));
        assert_eq!(n, "eggs");
    }

    #[test]
    fn preserve_multiword_name() {
        let (_, _, n) = qty_unit("1 cup all-purpose flour");
        assert_eq!(n, "all-purpose flour");
    }

    #[test]
    fn empty_line_uncertain() {
        let p = parse_ingredient_line("   ");
        assert!(p.uncertain);
        assert!(p.name.is_empty());
    }

    #[test]
    fn unicode_range_dash() {
        let p = parse_ingredient_line("1–2 tsp vanilla");
        assert_eq!(p.quantity, Some(1.5));
        assert!(p.uncertain);
    }

    #[test]
    fn to_taste_preserves_quantity() {
        let p = parse_ingredient_line("1 tsp salt, or to taste");
        assert_eq!(p.quantity, Some(1.0));
        assert_eq!(p.unit.as_ref().map(|u| u.kind), Some(UnitKind::Volume));
        assert!(p.name.to_lowercase().contains("salt"));
        assert!(
            !p.name
                .chars()
                .all(|c| c.is_lowercase() || !c.is_alphabetic())
                || p.name.contains("salt")
        );
        // Name should not be fully forced to a garbage lowercase of the whole line
        assert!(!p.name.contains("to taste"));
    }

    #[test]
    fn t_abbrev_is_teaspoon() {
        let p = parse_ingredient_line("1 t salt");
        assert_eq!(p.quantity, Some(1.0));
        let u = p.unit.unwrap();
        assert!(
            (u.to_base - 4.92892159375).abs() < 1e-6,
            "got {}",
            u.to_base
        );
    }

    #[test]
    fn uppercase_t_abbrev_is_tablespoon() {
        let p = parse_ingredient_line("1 T butter");
        assert_eq!(p.quantity, Some(1.0));
        let u = p.unit.unwrap();
        assert!(
            (u.to_base - 14.78676478125).abs() < 1e-6,
            "got {}",
            u.to_base
        );
    }

    #[test]
    fn unicode_fraction_standalone() {
        let (q, k, n) = qty_unit("¼ cup flour");
        assert_eq!(q, Some(0.25));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "flour");
    }

    #[test]
    fn unicode_fraction_glued() {
        let (q, k, n) = qty_unit("1½ cups milk");
        assert_eq!(q, Some(1.5));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "milk");
    }

    #[test]
    fn unicode_fraction_spaced() {
        let (q, _, n) = qty_unit("1 ½ cups milk");
        assert_eq!(q, Some(1.5));
        assert_eq!(n, "milk");
    }

    #[test]
    fn unicode_fraction_two_thirds() {
        let (q, k, _) = qty_unit("⅔ cup sugar");
        assert!((q.unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(k, Some(UnitKind::Volume));
    }

    #[test]
    fn ascii_mixed_number_still_parses() {
        let (q, _, n) = qty_unit("1 1/2 cups milk");
        assert_eq!(q, Some(1.5));
        assert_eq!(n, "milk");
    }

    #[test]
    fn parenthetical_size_single() {
        let (q, k, n) = qty_unit("1 (14 oz) can tomatoes");
        assert_eq!(q, Some(14.0));
        assert_eq!(k, Some(UnitKind::Mass));
        assert_eq!(n, "tomatoes");
    }

    #[test]
    fn parenthetical_size_multiplied_glued() {
        let (q, k, n) = qty_unit("2 (400g) cans tomatoes");
        assert_eq!(q, Some(800.0));
        assert_eq!(k, Some(UnitKind::Mass));
        assert_eq!(n, "tomatoes");
    }

    #[test]
    fn parenthetical_size_volume_carton() {
        let (q, k, n) = qty_unit("1 (500 ml) carton stock");
        assert_eq!(q, Some(500.0));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "stock");
    }

    #[test]
    fn parenthetical_non_size_not_treated_as_size() {
        // "(optional)" has no quantity+unit and must not be read as a package size.
        let p = parse_ingredient_line("1 (optional) onion");
        assert_eq!(p.quantity, Some(1.0));
        assert!(p.unit.is_none());
        assert!(p.name.contains("onion"));
    }

    #[test]
    fn trailing_parenthetical_note_unchanged() {
        let p = parse_ingredient_line("2 eggs (large)");
        assert_eq!(p.quantity, Some(2.0));
        assert_eq!(p.name, "eggs");
        assert_eq!(p.note.as_deref(), Some("large"));
    }

    #[test]
    fn to_taste_in_parentheses_no_dangling_paren() {
        let p = parse_ingredient_line("salt (to taste)");
        assert_eq!(p.name, "salt");
        assert!(p.quantity.is_none());
    }

    #[test]
    fn comma_descriptor_prefix_keeps_noun() {
        use crate::domain::IngredientKey;
        use crate::normalize::normalize_line;
        let p = parse_ingredient_line("2 skinless, boneless chicken breasts");
        assert_eq!(p.quantity, Some(2.0));
        // The real noun is retained.
        assert!(p.name.contains("chicken breast"), "got {:?}", p.name);
        let key = IngredientKey::from_line(&normalize_line("2 skinless, boneless chicken breasts"));
        assert_eq!(key.name, "chicken breasts");
    }

    #[test]
    fn comma_prep_note_still_splits() {
        // A prep note after the comma is separated.
        let p = parse_ingredient_line("2.5 lb chicken breast, diced");
        assert_eq!(p.name, "chicken breast");
        assert_eq!(p.note.as_deref(), Some("diced"));
    }

    #[test]
    fn word_number_needs_word_boundary() {
        // Leading "a"/"ten" must not be consumed from ordinary ingredient names.
        for line in ["avocado", "apple", "almonds", "tender greens", "onion"] {
            let p = parse_ingredient_line(line);
            assert_eq!(p.name, line, "{line} mis-parsed");
            assert_eq!(p.quantity, None, "{line} got a phantom quantity");
        }
        // Genuine word numbers parse.
        let p = parse_ingredient_line("one onion");
        assert_eq!(p.quantity, Some(1.0));
        assert_eq!(p.name, "onion");
    }

    #[test]
    fn to_taste_no_panic_on_multibyte_lowercase() {
        // U+212A KELVIN SIGN lowercases to ASCII 'k' (byte length shrinks); the
        // name-cut must stay on a char boundary.
        for line in ["\u{212A}é to taste", "İ salt to taste", "Köşe as needed"] {
            let p = parse_ingredient_line(line);
            assert!(p.quantity.is_none());
            assert!(!p.name.contains("to taste") && !p.name.contains("as needed"));
        }
    }

    #[test]
    fn leading_parenthetical_size_does_not_leak_into_name() {
        // A word-unit before the clarifier: "1 packet (.85 oz) …".
        let (q, _, n) = qty_unit("1 packet (.85 oz) Old El Paso Chicken Taco Seasoning Mix");
        assert_eq!(q, Some(1.0));
        assert_eq!(n, "Old El Paso Chicken Taco Seasoning Mix");
        // A real unit before the clarifier: the name is just the ingredient.
        let (q, k, n) = qty_unit("3 tablespoons (1 1/2 ounces) dry vermouth");
        assert_eq!((q, k), (Some(3.0), Some(UnitKind::Volume)));
        assert_eq!(n, "dry vermouth");
        // A non-quantity parenthetical ("(1 inch)") that isn't a package size.
        let (_, _, n) = qty_unit("1 (1 inch) piece fresh ginger");
        assert!(!n.contains('('), "name still had a paren: {n}");
        // Trailing clarifier is stripped too.
        let (_, _, n) = qty_unit("3 (0.33-ounce) packets red popping candy (such as pop rocks)");
        assert!(!n.contains('(') && !n.contains(')'), "{n}");
    }

    #[test]
    fn compound_plus_quantity_drops_the_addend_from_name() {
        let (q, k, n) = qty_unit("1 cup + 3 tablespoons beef broth (divided)");
        assert_eq!((q, k), (Some(1.0), Some(UnitKind::Volume)));
        assert_eq!(n, "beef broth");
        let (_, _, n) = qty_unit("1 + 1/2 cups unsweetened pineapple chunks, drained");
        assert_eq!(n, "unsweetened pineapple chunks");
    }

    #[test]
    fn leading_stray_punctuation_is_trimmed() {
        // Leading stray punctuation is dropped from the name.
        assert_eq!(qty_unit(". frozen cranberries").2, "frozen cranberries");
        assert_eq!(qty_unit("- ice cubes").2, "ice cubes");
    }

    #[test]
    fn html_entity_fraction_becomes_a_quantity() {
        // "&#8531;" is ⅓; decoding it yields the quantity.
        let (q, k, n) = qty_unit("&#8531; cup chopped fresh cilantro");
        assert_eq!(q, Some(1.0 / 3.0));
        assert_eq!(k, Some(UnitKind::Volume));
        assert_eq!(n, "chopped fresh cilantro");
        // Entity apostrophe folds to ASCII in the name.
        let (_, _, n) = qty_unit("2 cups jalape\u{00f1}o&#8217;s");
        assert_eq!(n, "jalape\u{00f1}o's");
    }
}
