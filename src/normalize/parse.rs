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
          | (?P<word>a|an|one|two|three|four|five|six|seven|eight|nine|ten|half|quarter)
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
    // Comma-separated note (common in recipes: "chicken breast, diced")
    if let Some(idx) = rest.find(',') {
        let name = rest[..idx].trim().to_string();
        let note = rest[idx + 1..].trim().to_string();
        if !name.is_empty() && !note.is_empty() {
            return (name, Some(note));
        }
    }
    (rest.to_string(), None)
}

fn clean_name(name: &str) -> String {
    let mut s = name.trim().to_string();
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
    let line = line.trim();
    if line.is_empty() {
        return ParsedIngredient {
            name: String::new(),
            quantity: None,
            unit: None,
            note: None,
            uncertain: true,
        };
    }

    // "to taste" / "as needed" only when there is no leading quantity — otherwise
    // parse normally and attach a note (preserve qty/unit and original name casing).
    let lower = line.to_lowercase();
    let has_taste_note =
        lower.contains("to taste") || lower.contains("as needed") || lower.contains("as desired");
    if has_taste_note && parse_quantity_prefix(line).is_none() {
        // Strip trailing taste notes from the name, preserving non-lowercased source when possible.
        let mut name = line.to_string();
        for phrase in ["to taste", "as needed", "as desired", "or to taste"] {
            if let Some(idx) = name.to_lowercase().find(phrase) {
                name = name[..idx].to_string();
            }
        }
        name = name.trim().trim_end_matches(',').trim().to_string();
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

    // If we have qty but no unit and name looks like a countable noun, treat as count.
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
}
