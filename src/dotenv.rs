//! Minimal `.env` loader.
//!
//! Sets `KEY=VALUE` pairs from a `.env` file in the current directory into the
//! process environment, without overriding variables already set (a real
//! `export` wins over the file). Intentionally tiny — enough to keep secrets
//! like `SMARTER_RECIPES_FDC_KEY` out of shell history and the repo.

use std::path::Path;

/// Load `.env` from the current working directory if present. Missing file is
/// fine (no-op). Call once at startup, before any threads spawn.
pub fn load() {
    load_from(Path::new(".env"));
}

fn load_from(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (key, value) in parse(&text) {
        if std::env::var_os(&key).is_none() {
            std::env::set_var(&key, value);
        }
    }
}

/// Parse `.env` text into `(key, value)` pairs. Skips blank lines and `#`
/// comments, tolerates an `export ` prefix, and strips matching surrounding
/// single/double quotes from the value.
fn parse(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        out.push((key.to_string(), strip_quotes(value.trim()).to_string()));
    }
    out
}

fn strip_quotes(v: &str) -> &str {
    let b = v.as_bytes();
    if v.len() >= 2
        && ((b[0] == b'"' && b[v.len() - 1] == b'"') || (b[0] == b'\'' && b[v.len() - 1] == b'\''))
    {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn parses_pairs_quotes_comments_and_export() {
        let text = "\n\
            # a comment\n\
            SMARTER_RECIPES_FDC_KEY=abc123\n\
            export FOO=\"bar baz\"\n\
            SQ='single'\n\
            EMPTY=\n\
            no_equals_line\n\
            =novalue\n\
            spaced =  trimmed \n";
        let m: HashMap<_, _> = parse(text).into_iter().collect();
        assert_eq!(m["SMARTER_RECIPES_FDC_KEY"], "abc123");
        assert_eq!(m["FOO"], "bar baz");
        assert_eq!(m["SQ"], "single");
        assert_eq!(m["EMPTY"], "");
        assert_eq!(m["spaced"], "trimmed");
        assert!(!m.contains_key("no_equals_line"));
        assert!(!m.contains_key(""));
    }

    #[test]
    fn load_from_sets_missing_but_keeps_existing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "SR_DOTENV_NEW=fromfile\nSR_DOTENV_SET=fromfile\n").unwrap();
        // A var already in the environment must win over the file.
        std::env::set_var("SR_DOTENV_SET", "preset");
        load_from(&path);
        assert_eq!(std::env::var("SR_DOTENV_NEW").unwrap(), "fromfile");
        assert_eq!(std::env::var("SR_DOTENV_SET").unwrap(), "preset");
        std::env::remove_var("SR_DOTENV_NEW");
        std::env::remove_var("SR_DOTENV_SET");
    }
}
