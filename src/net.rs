//! Small shared HTTP helpers.

/// Percent-encode a string for use as a URL query-parameter value. Unreserved
/// characters (RFC 3986) pass through; everything else, including non-ASCII, is
/// encoded per UTF-8 byte. Spaces become `%20`.
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
}
