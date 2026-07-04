//! Pure text-cleaning helpers for scraped recipe content: HTML-entity decoding
//! and typographic normalization. No I/O — safe to use from any layer.

/// Decode HTML entities, then fold typographic punctuation to ASCII. Scraped
/// recipe text (titles, ingredient lines) routinely carries both — a title like
/// `S&#8217;mores` or a curly `S'mores` should both read as `S'mores`, and an
/// ingredient like `&#8531; cup` should become `⅓ cup` so the quantity parser
/// can see the fraction.
pub fn sanitize(s: &str) -> String {
    fold_typography(&decode_html_entities(s))
}

/// Replace curly quotes/apostrophes with their ASCII equivalents so text
/// compares and displays consistently (`S'mores` → `S'mores`, `"x"` → `"x"`).
/// Leaves everything else — including dashes, which the quantity parser handles
/// on its own — untouched.
pub fn fold_typography(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{2032}' => '\'', // ‘ ’ ′
            '\u{201C}' | '\u{201D}' | '\u{2033}' => '"',  // “ ” ″
            other => other,
        })
        .collect()
}

/// Decode the HTML entities that commonly leak into scraped recipe text.
/// JSON-LD strings are plain JSON (not HTML-decoded), so sites that embed
/// `&nbsp;`, `&amp;`, etc. in `recipeIngredient` values pass them through
/// verbatim. Handles the common named entities plus numeric `&#N;` / `&#xN;`;
/// unrecognized `&…;` sequences are left untouched.
pub fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        // Look for a ';' close by (entity names/refs are short).
        let semi = after[1..]
            .find(';')
            .map(|p| p + 1)
            .filter(|&p| (2..=12).contains(&p));
        match semi.and_then(|p| decode_entity(&after[1..p]).map(|d| (d, p))) {
            Some((decoded, p)) => {
                out.push_str(&decoded);
                rest = &after[p + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode the body of one entity (the text between `&` and `;`).
fn decode_entity(e: &str) -> Option<String> {
    let named = match e {
        "nbsp" => ' ', // fold to a plain space; whitespace-collapsing handles it
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "rsquo" | "lsquo" => '\u{2019}', // folded to ASCII by fold_typography
        "rdquo" | "ldquo" => '\u{201D}',
        "deg" => '°',
        "mdash" => '—',
        "ndash" => '–',
        "reg" => '®',
        "trade" => '™',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "frac13" => '⅓',
        "frac23" => '⅔',
        "hellip" => '…',
        _ => return decode_numeric_entity(e),
    };
    Some(named.to_string())
}

fn decode_numeric_entity(e: &str) -> Option<String> {
    let num = e.strip_prefix('#')?;
    let code = match num.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => num.parse::<u32>().ok()?,
    };
    char::from_u32(code).map(|c| c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_entities() {
        assert_eq!(decode_html_entities("salt&nbsp;"), "salt ");
        assert_eq!(decode_html_entities("salt &amp; pepper"), "salt & pepper");
        assert_eq!(decode_html_entities("a&#39;b"), "a'b");
        assert_eq!(decode_html_entities("&#x2153; cup"), "⅓ cup");
        assert_eq!(decode_html_entities("2&frac12; cups"), "2½ cups");
    }

    #[test]
    fn leaves_plain_and_unknown_text_untouched() {
        assert_eq!(decode_html_entities("olive oil"), "olive oil");
        assert_eq!(decode_html_entities("A&B Foods"), "A&B Foods");
        assert_eq!(decode_html_entities("m&unknown; n"), "m&unknown; n");
        assert_eq!(decode_html_entities("trailing &"), "trailing &");
    }

    #[test]
    fn sanitize_fixes_entity_and_curly_apostrophes() {
        // The S'mores case: numeric entity for a right single quote → ASCII '.
        assert_eq!(sanitize("S&#8217;mores Fudge"), "S'mores Fudge");
        // A raw curly apostrophe (no entity) is folded too.
        assert_eq!(sanitize("S\u{2019}mores"), "S'mores");
        // Entity fraction becomes a real fraction char for the quantity parser.
        assert_eq!(sanitize("&#8531; cup cilantro"), "⅓ cup cilantro");
        assert_eq!(
            sanitize("chunky bean &amp; corn salsa"),
            "chunky bean & corn salsa"
        );
    }

    #[test]
    fn fold_typography_is_ascii_fast_path() {
        assert_eq!(fold_typography("plain ascii"), "plain ascii");
        assert_eq!(fold_typography("\u{201C}quoted\u{201D}"), "\"quoted\"");
    }
}
