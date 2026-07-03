//! Small shared HTTP helpers.

/// Percent-encode a string for use as a URL query-parameter value. Unreserved
/// characters (RFC 3986) pass through; everything else, including non-ASCII, is
/// encoded per UTF-8 byte. Spaces become `%20` (valid in query values on every
/// server we target).
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
        "deg" => '°',
        "mdash" => '—',
        "ndash" => '–',
        "reg" => '®',
        "trade" => '™',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
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
    fn encodes_spaces_and_unicode() {
        assert_eq!(encode_query("olive oil"), "olive%20oil");
        assert_eq!(encode_query("crème"), "cr%C3%A8me");
        assert_eq!(encode_query("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_query("a&b=c"), "a%26b%3Dc");
    }

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
}
