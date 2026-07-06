//! EPUB cookbook ingestion via a linked recipe index (or TOC fallback).
//!
//! Pipeline: open book → find index (or TOC) → collect linked starts →
//! segment HTML by fragment/spine order → Ingredients/Steps heuristics →
//! [`Recipe`] values filtered by [`super::is_cookable`].

use super::{epub_source_key_for_resolved, is_cookable, resolve_epub_path, text_has_amount};
use crate::domain::{Recipe, RecipeMeta, RecipeSource};
use crate::normalize::normalize_line;
use crate::text::sanitize;
use anyhow::{bail, Context, Result};
use epub::doc::EpubDoc;
use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};

/// One linked start from a recipe index or TOC leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub title: String,
    /// Resource path relative to the EPUB root (as used by `epub` crate paths).
    pub path: String,
    pub fragment: Option<String>,
    /// Original href string from the index (for provenance).
    pub href: String,
}

/// Ingest all cookable recipes from an EPUB path.
pub fn ingest_epub(path: &str) -> Result<Vec<Recipe>> {
    let doc = EpubDoc::new(path).with_context(|| format!("opening EPUB {path}"))?;
    ingest_epub_doc(doc, path)
}

fn ingest_epub_doc<R: Read + Seek>(mut doc: EpubDoc<R>, path: &str) -> Result<Vec<Recipe>> {
    let entries = collect_entries(&mut doc)?;
    if entries.is_empty() {
        bail!(
            "no linked recipe entries found in EPUB index or TOC; \
             need a recipe index with hyperlinks (page-number-only indexes unsupported)"
        );
    }

    // Canonicalize once per book — every recipe identity key shares this path.
    let resolved_epub_path = resolve_epub_path(path);

    let spine_order = spine_path_order(&doc);
    let mut by_path: HashMap<String, Vec<IndexEntry>> = HashMap::new();
    for e in entries {
        by_path.entry(e.path.clone()).or_default().push(e);
    }

    // Process resources in spine order; within a file, fragment order.
    let mut path_keys: Vec<String> = by_path.keys().cloned().collect();
    path_keys.sort_by_key(|p| spine_order.get(p).copied().unwrap_or(usize::MAX));

    let mut out = Vec::new();
    let mut seen_href: HashSet<String> = HashSet::new();
    let mut missing_resources = 0usize;
    let mut loaded_any = false;
    let mut skipped_unresolved = 0usize;

    for res_path in path_keys {
        let mut group = by_path.remove(&res_path).unwrap_or_default();
        let html = match doc.get_resource_str_by_path(Path::new(&res_path)) {
            Some(s) => s,
            None => {
                missing_resources += 1;
                continue;
            }
        };
        loaded_any = true;
        // Order entries by first occurrence of fragment in HTML (unresolved last).
        // Cached key so fragment_offset runs once per entry, not O(n log n) times.
        group.sort_by_cached_key(|e| {
            fragment_offset(&html, e.fragment.as_deref()).unwrap_or(usize::MAX)
        });

        let entry_count = group.len();
        let segments = segment_html(&html, &group);
        if segments.len() < entry_count {
            skipped_unresolved += entry_count - segments.len();
        }
        for (entry, segment_html) in segments {
            // Dedup by locator (path+fragment/href), not display title — cookbooks
            // often reuse titles like "Vinaigrette".
            let loc_key = format!(
                "{}#{}",
                entry.path,
                entry.fragment.as_deref().unwrap_or(entry.href.as_str())
            );
            if !seen_href.insert(loc_key) {
                continue;
            }
            let recipe = html_segment_to_recipe(&entry, &segment_html, path, &resolved_epub_path);
            if is_cookable(&recipe) {
                out.push(recipe);
            }
        }
    }

    if skipped_unresolved > 0 {
        eprintln!(
            "note: skipped {skipped_unresolved} EPUB index entr{} with unresolved fragment anchors",
            if skipped_unresolved == 1 { "y" } else { "ies" }
        );
    }

    if !loaded_any && missing_resources > 0 {
        bail!(
            "EPUB index/TOC pointed at {missing_resources} resource path(s) that could not be read"
        );
    }

    if out.is_empty() {
        bail!(
            "EPUB index/TOC produced entries but none looked cookable \
             (need ≥2 ingredient lines with amounts)"
        );
    }
    Ok(out)
}

fn collect_entries<R: Read + Seek>(doc: &mut EpubDoc<R>) -> Result<Vec<IndexEntry>> {
    if let Some(index_path) = find_index_path(doc) {
        if let Some(html) = doc.get_resource_str_by_path(Path::new(&index_path)) {
            let from_index = parse_index_entries(&html, &index_path);
            if !from_index.is_empty() {
                return Ok(from_index);
            }
        }
    }
    // Fallback: scan all XHTML for the best index-like document.
    if let Some(entries) = best_index_scan(doc) {
        if !entries.is_empty() {
            return Ok(entries);
        }
    }
    Ok(entries_from_toc(doc))
}

fn find_index_path<R: Read + Seek>(doc: &mut EpubDoc<R>) -> Option<String> {
    // Prefer resources whose path or id suggests an index.
    let mut candidates: Vec<(i32, String)> = Vec::new();
    for item in &doc.spine {
        let Some(res) = doc.resources.get(&item.idref) else {
            continue;
        };
        let path_str = path_to_string(&res.path);
        let lower = path_str.to_lowercase();
        let score = index_path_score(&lower);
        if score > 0 {
            candidates.push((score, path_str));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    // TOC label "Index" only wins if that document actually has linked entries.
    let toc_index_paths: Vec<String> = flatten_nav(&doc.toc)
        .into_iter()
        .filter(|np| {
            let label = np.label.to_lowercase();
            label.contains("recipe index") || label == "index" || label.contains("index of recipes")
        })
        .map(|np| {
            let path = path_to_string(&np.content);
            split_path_fragment(&path).0
        })
        .collect();
    for path in toc_index_paths {
        if let Some(html) = doc.get_resource_str_by_path(Path::new(&path)) {
            if !parse_index_entries(&html, &path).is_empty() {
                return Some(path);
            }
        }
    }

    candidates.into_iter().map(|(_, p)| p).next()
}

fn index_path_score(lower_path: &str) -> i32 {
    let file = Path::new(lower_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(lower_path);
    if file == "recipe-index" || file == "recipe_index" || file == "recipes-index" {
        return 100;
    }
    if file == "index" || file.ends_with("-index") || file.ends_with("_index") {
        return 80;
    }
    if file.contains("recipe") && file.contains("index") {
        return 90;
    }
    if file.contains("index") {
        return 40;
    }
    0
}

fn best_index_scan<R: Read + Seek>(doc: &mut EpubDoc<R>) -> Option<Vec<IndexEntry>> {
    let mut best: Option<(usize, Vec<IndexEntry>)> = None;
    for item in doc.spine.clone() {
        let Some(res) = doc.resources.get(&item.idref) else {
            continue;
        };
        if !res.mime.contains("html") && !res.mime.contains("xml") {
            continue;
        }
        let path_str = path_to_string(&res.path);
        let Some(html) = doc.get_resource_str_by_path(Path::new(&path_str)) else {
            continue;
        };
        let entries = parse_index_entries(&html, &path_str);
        // Index-like: several internal links, not a huge prose chapter.
        if entries.len() < 2 {
            continue;
        }
        let text_len = html_visible_len(&html);
        // Prefer many links with relatively little body text.
        let score = entries
            .len()
            .saturating_mul(1000)
            .saturating_sub(text_len / 20);
        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, entries));
        }
    }
    best.map(|(_, e)| e)
}

fn entries_from_toc<R: Read + Seek>(doc: &EpubDoc<R>) -> Vec<IndexEntry> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for np in flatten_nav(&doc.toc) {
        let title = sanitize(np.label.trim());
        if title.is_empty() || is_noise_title(&title) {
            continue;
        }
        let path = path_to_string(&np.content);
        // Nav content may include a fragment already in the PathBuf display.
        let (path, fragment) = split_path_fragment(&path);
        let href = match &fragment {
            Some(f) => format!("{path}#{f}"),
            None => path.clone(),
        };
        let key = format!("{path}#{}", fragment.as_deref().unwrap_or(""));
        if !seen.insert(key) {
            continue;
        }
        out.push(IndexEntry {
            title,
            path,
            fragment,
            href,
        });
    }
    out
}

fn flatten_nav(points: &[epub::doc::NavPoint]) -> Vec<&epub::doc::NavPoint> {
    let mut out = Vec::new();
    fn walk<'a>(points: &'a [epub::doc::NavPoint], out: &mut Vec<&'a epub::doc::NavPoint>) {
        for p in points {
            if p.children.is_empty() {
                out.push(p);
            } else {
                // Prefer leaves; also include parent if it has its own content target
                // distinct from first child (rare). For cookbooks, leaves matter.
                walk(&p.children, out);
            }
        }
    }
    walk(points, &mut out);
    out
}

/// Parse `<a href>` entries from an index HTML document.
pub fn parse_index_entries(html: &str, base_path: &str) -> Vec<IndexEntry> {
    let document = Html::parse_document(html);
    let Ok(sel) = Selector::parse("a[href]") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for a in document.select(&sel) {
        let href = a.value().attr("href").unwrap_or("").trim();
        if href.is_empty() || href.starts_with("http://") || href.starts_with("https://") {
            continue;
        }
        if href.starts_with("mailto:") || href.starts_with("javascript:") {
            continue;
        }
        let title = sanitize(&a.text().collect::<String>());
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() || is_noise_title(&title) {
            continue;
        }
        // Skip pure page-number labels ("12", "xiv", "XII").
        let title_lc = title.to_ascii_lowercase();
        if title_lc
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'x' | 'i' | 'v'))
            && title_lc.len() < 6
        {
            continue;
        }
        let (path, fragment) = resolve_href(base_path, href);
        if path.is_empty() {
            continue;
        }
        let key = format!(
            "{}#{}|{}",
            path,
            fragment.as_deref().unwrap_or(""),
            normalize_title_key(&title)
        );
        if !seen.insert(key) {
            continue;
        }
        out.push(IndexEntry {
            title,
            path,
            fragment,
            href: href.to_string(),
        });
    }
    out
}

fn is_noise_title(title: &str) -> bool {
    // Single-line pattern: raw-string `\`+newline is literal, not a continuation.
    static NOISE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(cover|title page|copyright|contents|table of contents|toc|acknowledgements?|acknowledgments?|foreword|preface|introduction|about the author|bibliography|glossary|notes?|dedication|half title|also by|colophon)s?$",
        )
        .expect("noise re")
    });
    let t = title.trim();
    if t.len() < 2 {
        return true;
    }
    NOISE.is_match(t)
}

/// Resolve a relative href against the index document path.
pub fn resolve_href(base_path: &str, href: &str) -> (String, Option<String>) {
    let href = href.trim();
    let (path_part, fragment) = match href.split_once('#') {
        Some((p, f)) => (
            p,
            if f.is_empty() {
                None
            } else {
                Some(f.to_string())
            },
        ),
        None => (href, None),
    };
    if path_part.is_empty() {
        // Same-document fragment.
        return (normalize_epub_path(base_path), fragment);
    }
    let base_dir = Path::new(base_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let joined = base_dir.join(path_part);
    (normalize_epub_path_buf(&joined), fragment)
}

fn split_path_fragment(s: &str) -> (String, Option<String>) {
    match s.split_once('#') {
        Some((p, f)) => (
            normalize_epub_path(p),
            if f.is_empty() {
                None
            } else {
                Some(f.to_string())
            },
        ),
        None => (normalize_epub_path(s), None),
    }
}

fn normalize_epub_path(s: &str) -> String {
    normalize_epub_path_buf(Path::new(s))
}

fn normalize_epub_path_buf(p: &Path) -> String {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    path_to_string(&out)
}

fn path_to_string(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn spine_path_order<R: Read + Seek>(doc: &EpubDoc<R>) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for (i, item) in doc.spine.iter().enumerate() {
        if let Some(res) = doc.resources.get(&item.idref) {
            m.insert(path_to_string(&res.path), i);
        }
    }
    m
}

/// Byte offset of an element `id`/`name` attribute with an **exact** fragment value.
///
/// - No per-call `Regex::new` (used inside sorts / multi-entry segmentation).
/// - Avoids prefix false-positives (`id="a"` matching inside `id="apple"`).
/// - Avoids `data-id` / `xml:id` false matches (attr name must be a full token).
fn fragment_offset(html: &str, fragment: Option<&str>) -> Option<usize> {
    let frag = fragment.filter(|f| !f.is_empty())?;
    find_attr_value_offset(html, "id", frag).or_else(|| find_attr_value_offset(html, "name", frag))
}

/// Find `attr="value"` / `attr='value'` / `attr=value` (ASCII case-insensitive attr name).
fn find_attr_value_offset(html: &str, attr: &str, value: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let attr_bytes = attr.as_bytes();
    if attr_bytes.is_empty() || bytes.len() < attr_bytes.len() {
        return None;
    }
    let mut i = 0;
    while i + attr_bytes.len() <= bytes.len() {
        if !eq_ascii_ignore_case_prefix(&bytes[i..], attr_bytes) {
            i += 1;
            continue;
        }
        // Full attribute-name token: reject `data-id`, `xml:id`, `my_id`, etc.
        if i > 0 {
            let prev = bytes[i - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'-' || prev == b':' {
                i += 1;
                continue;
            }
        }
        let mut j = i + attr_bytes.len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            i += 1;
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let matched = match bytes[j] {
            b'"' => {
                let end = memchr_byte(bytes, j + 1, b'"')?;
                &html[j + 1..end]
            }
            b'\'' => {
                let end = memchr_byte(bytes, j + 1, b'\'')?;
                &html[j + 1..end]
            }
            _ => {
                let start = j;
                while j < bytes.len()
                    && !bytes[j].is_ascii_whitespace()
                    && bytes[j] != b'>'
                    && bytes[j] != b'/'
                {
                    j += 1;
                }
                &html[start..j]
            }
        };
        if matched == value {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn eq_ascii_ignore_case_prefix(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len()
        && hay[..needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn memchr_byte(bytes: &[u8], from: usize, b: u8) -> Option<usize> {
    bytes[from..].iter().position(|&c| c == b).map(|p| from + p)
}

/// Split HTML into per-entry segments using fragment anchors.
///
/// - Single entry → whole document.
/// - Multiple entries, **no** resolvable fragments → one segment only (first entry),
///   to avoid N clones of the same chapter.
/// - Multiple entries with some resolvable fragments → cut between resolved offsets;
///   entries whose fragment never appears are dropped.
pub fn segment_html(html: &str, entries: &[IndexEntry]) -> Vec<(IndexEntry, String)> {
    if entries.is_empty() {
        return Vec::new();
    }
    if entries.len() == 1 {
        return vec![(entries[0].clone(), html.to_string())];
    }

    let mut resolved: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| fragment_offset(html, e.fragment.as_deref()).map(|off| (off, i)))
        .collect();

    if resolved.is_empty() {
        // Cannot split: emit a single full-document candidate under the first title.
        return vec![(entries[0].clone(), html.to_string())];
    }

    resolved.sort_by_key(|(off, i)| (*off, *i));
    // Dedupe identical offsets (keep first entry at that cut).
    resolved.dedup_by_key(|(off, _)| *off);

    let mut out = Vec::new();
    for (pos, &(start, entry_i)) in resolved.iter().enumerate() {
        let end = resolved
            .get(pos + 1)
            .map(|(next, _)| *next)
            .unwrap_or(html.len());
        if start >= end {
            continue;
        }
        out.push((entries[entry_i].clone(), html[start..end].to_string()));
    }
    out
}

fn html_segment_to_recipe(
    entry: &IndexEntry,
    segment_html: &str,
    epub_path: &str,
    resolved_epub_path: &str,
) -> Recipe {
    let text = html_to_recipe_text(segment_html, &entry.title);
    let mut recipe = plain_recipe_from_text(&text, &entry.title);
    recipe.source = RecipeSource::Epub {
        path: epub_path.to_string(),
        href: entry.href.clone(),
    };
    recipe.meta = RecipeMeta {
        notes: Some(format!("Imported from EPUB index entry: {}", entry.href)),
        // Query form — fragments would be stripped by normalize_url dedup.
        // Path component reused from a single canonicalize per book.
        source_url: Some(epub_source_key_for_resolved(
            resolved_epub_path,
            &entry.href,
        )),
        ..Default::default()
    };
    recipe
}

/// Convert a recipe HTML fragment into plain text with Ingredients:/Steps: markers.
///
/// Index/`fallback_title` is the recipe title (always set for index-driven import).
/// Pre-header prose (headnotes) is kept in the body section of the text;
/// [`plain_recipe_from_text`] discards it once an Ingredients/Steps header appears.
pub fn html_to_recipe_text(html: &str, fallback_title: &str) -> String {
    let body_text = element_lines(html);
    let mut out = Vec::new();
    out.push(fallback_title.to_string());
    let mut section = "body";
    for line in body_text {
        let lower = line.to_lowercase();
        if is_ingredients_header(&lower) {
            out.push("Ingredients:".into());
            section = "ingredients";
            continue;
        }
        if is_steps_header(&lower) {
            out.push("Steps:".into());
            section = "steps";
            continue;
        }
        // Skip repeating the title line (common when the heading is also in body text).
        if section == "body" && normalize_title_key(&line) == normalize_title_key(fallback_title) {
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

fn is_ingredients_header(lower: &str) -> bool {
    let t = lower.trim().trim_end_matches(':').trim();
    matches!(
        t,
        "ingredients" | "ingredient" | "you will need" | "what you need" | "shopping list"
    )
}

fn is_steps_header(lower: &str) -> bool {
    let t = lower.trim().trim_end_matches(':').trim();
    matches!(
        t,
        "steps"
            | "step"
            | "instructions"
            | "instruction"
            | "directions"
            | "direction"
            | "method"
            | "preparation"
            | "procedure"
            | "how to make it"
            | "to make"
    )
}

/// Extract visible text lines from HTML, preferring list items.
fn element_lines(html: &str) -> Vec<String> {
    let document = Html::parse_fragment(html);
    let mut lines = Vec::new();
    // Walk block-ish elements.
    if let Ok(sel) =
        Selector::parse("h1, h2, h3, h4, p, li, dt, dd, div.ingredient, span.ingredient")
    {
        for el in document.select(&sel) {
            let has_block_child = el
                .children()
                .filter_map(|n| n.value().as_element())
                .any(|e| is_block_level_name(e.name()));
            // With nested blocks (e.g. `<li>text<ul>…`), take this node's text plus
            // inline descendants; nested blocks are selected separately.
            let t = if has_block_child {
                element_own_text(el)
            } else {
                sanitize(&el.text().collect::<String>())
            };
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.is_empty() {
                continue;
            }
            lines.push(t);
        }
    }
    if lines.is_empty() {
        // Fallback: all text collapsed by lines from raw strip.
        let stripped = strip_tags(html);
        for line in stripped.lines() {
            let t = line.trim();
            if !t.is_empty() {
                lines.push(sanitize(t));
            }
        }
    }
    lines
}

/// Text for a block that also has nested block children: include this node's
/// text and **inline** descendants (e.g. `<i>sifted</i>`), but stop at nested
/// **block** elements (those are selected / emitted separately).
fn element_own_text(el: ElementRef<'_>) -> String {
    let mut parts = Vec::new();
    collect_inline_text(el, &mut parts);
    sanitize(&parts.join(" "))
}

fn is_block_level_name(name: &str) -> bool {
    matches!(
        name,
        "h1" | "h2"
            | "h3"
            | "h4"
            | "p"
            | "li"
            | "ul"
            | "ol"
            | "div"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "td"
            | "th"
            | "section"
            | "article"
            | "blockquote"
            | "pre"
            | "hr"
            | "figure"
            | "figcaption"
    )
}

fn collect_inline_text(el: ElementRef<'_>, parts: &mut Vec<String>) {
    for child in el.children() {
        if let Some(t) = child.value().as_text() {
            // Preserve internal spaces; only skip pure whitespace nodes.
            if !t.trim().is_empty() {
                parts.push(t.to_string());
            }
            continue;
        }
        let Some(elem) = child.value().as_element() else {
            continue;
        };
        if is_block_level_name(elem.name()) {
            // Nested block is handled by its own selector pass.
            continue;
        }
        // Inline (or unknown non-block) element: take its visible text tree.
        if let Some(child_el) = ElementRef::wrap(child) {
            collect_inline_text(child_el, parts);
        }
    }
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Script/style not handled specially — fine for cookbook XHTML fixtures.
    out
}

fn html_visible_len(html: &str) -> usize {
    strip_tags(html).split_whitespace().map(|w| w.len()).sum()
}

/// Same structure as file plain-text import.
///
/// **Headnotes:** prose before the first Ingredients header is not stored as
/// ingredients. When the body is promoted to ingredients (no `Ingredients:`
/// section — either headerless, or Steps/Method/Directions only), leading
/// non-amount lines are stripped up to the first amount-bearing line (same
/// heuristic as [`text_has_amount`] / `is_cookable`). That keeps common
/// cookbook layouts like *title → bullets → Directions:* cookable while
/// dropping leading headnote prose.
pub fn plain_recipe_from_text(text: &str, title_override: &str) -> Recipe {
    let mut title = title_override.to_string();
    let mut ingredients = Vec::new();
    let mut steps = Vec::new();
    let mut section = "title";
    let mut saw_ingredients_header = false;
    // Buffered body before Ingredients/Steps; may become ingredients when no
    // Ingredients: section is declared (with leading headnote stripped).
    let mut pre_header_body: Vec<String> = Vec::new();

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_lowercase();
        if is_ingredients_header(&lower) || (lower.starts_with("ingredients") && t.contains(':')) {
            section = "ingredients";
            saw_ingredients_header = true;
            continue;
        }
        if is_steps_header(&lower)
            || lower.starts_with("steps")
            || lower.starts_with("instructions")
            || lower.starts_with("directions")
        {
            section = "steps";
            continue;
        }
        match section {
            "title" => {
                if title_override.is_empty() {
                    title = t.to_string();
                }
                section = "body";
            }
            "ingredients" => {
                ingredients.push(normalize_line(t.trim_start_matches(['-', '*', '•'])));
            }
            "steps" => steps.push(
                t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
                    .trim()
                    .to_string(),
            ),
            _ => {
                // Headnote / ingredient bullets before headers — buffer only.
                pre_header_body.push(t.trim_start_matches(['-', '*', '•']).to_string());
            }
        }
    }

    // No Ingredients: section → promote buffered body, stripping leading headnote
    // prose (non-amount lines before the first amount-bearing line). Covers both
    // headerless lists and title → [headnote] → bullets → Method/Directions.
    if !saw_ingredients_header && ingredients.is_empty() && !pre_header_body.is_empty() {
        let kept = strip_leading_headnote_lines(&pre_header_body);
        ingredients = kept.into_iter().map(normalize_line).collect();
    }

    let mut recipe = Recipe::new(sanitize(&title));
    recipe.ingredients = ingredients;
    recipe.steps = steps;
    recipe
}

/// Drop leading non-amount prose until the first amount-bearing line; keep the rest.
/// If no line has an amount signal, keep all lines (cannot find a headnote boundary).
fn strip_leading_headnote_lines(lines: &[String]) -> Vec<&str> {
    match lines.iter().position(|l| text_has_amount(l)) {
        Some(i) => lines[i..].iter().map(String::as_str).collect(),
        None => lines.iter().map(String::as_str).collect(),
    }
}

fn normalize_title_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    #[test]
    fn parse_index_entries_resolves_relative_and_fragments() {
        let html = r#"
        <html><body>
          <h1>Recipe Index</h1>
          <ul>
            <li><a href="recipes.xhtml#pancakes">Fluffy Pancakes</a></li>
            <li><a href="recipes.xhtml#omelette">Simple Omelette</a></li>
            <li><a href="https://example.com/x">External</a></li>
            <li><a href="recipes.xhtml#pancakes">12</a></li>
          </ul>
        </body></html>
        "#;
        let entries = parse_index_entries(html, "OEBPS/Text/index.xhtml");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "Fluffy Pancakes");
        assert_eq!(entries[0].path, "OEBPS/Text/recipes.xhtml");
        assert_eq!(entries[0].fragment.as_deref(), Some("pancakes"));
        assert_eq!(entries[1].fragment.as_deref(), Some("omelette"));
    }

    #[test]
    fn same_document_fragment_href() {
        let (path, frag) = resolve_href("OEBPS/Text/index.xhtml", "#salad");
        assert_eq!(path, "OEBPS/Text/index.xhtml");
        assert_eq!(frag.as_deref(), Some("salad"));
    }

    #[test]
    fn segment_html_splits_on_ids() {
        let html = r#"
        <html><body>
          <section id="a"><h1>A</h1><p>one</p></section>
          <section id="b"><h1>B</h1><p>two</p></section>
        </body></html>
        "#;
        let entries = vec![
            IndexEntry {
                title: "A".into(),
                path: "x".into(),
                fragment: Some("a".into()),
                href: "#a".into(),
            },
            IndexEntry {
                title: "B".into(),
                path: "x".into(),
                fragment: Some("b".into()),
                href: "#b".into(),
            },
        ];
        let segs = segment_html(html, &entries);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].1.contains("one"));
        assert!(!segs[0].1.contains("two"));
        assert!(segs[1].1.contains("two"));
    }

    #[test]
    fn fragment_offset_exact_not_prefix() {
        let html = r#"<section id="apple">x</section><section id="a">y</section>"#;
        assert!(
            fragment_offset(html, Some("a")).unwrap()
                > fragment_offset(html, Some("apple")).unwrap()
        );
        // "a" must not resolve to the start of id="apple"
        let off_a = fragment_offset(html, Some("a")).unwrap();
        assert!(html[off_a..].starts_with("id=\"a\"") || html[off_a..].contains("id=\"a\""));
        assert!(!html[off_a..off_a + 10.min(html.len() - off_a)].contains("apple"));
    }

    #[test]
    fn fragment_offset_ignores_data_id() {
        let html = r#"<div data-id="frag">nope</div><section id="frag">yes</section>"#;
        let off = fragment_offset(html, Some("frag")).unwrap();
        assert!(
            html[off..].starts_with("id=\"frag\""),
            "matched at {:?}: {}",
            off,
            &html[off..off + 20.min(html.len() - off)]
        );
    }

    #[test]
    fn noise_title_matches_foreword_dedication_acknowledgements() {
        assert!(is_noise_title("Foreword"));
        assert!(is_noise_title("Dedication"));
        assert!(is_noise_title("Acknowledgements"));
        assert!(is_noise_title("Acknowledgments"));
        assert!(!is_noise_title("Fluffy Pancakes"));
    }

    #[test]
    fn headnote_prose_not_stored_as_ingredients() {
        let html = r#"
          <section id="stew">
            <h1>Grandma's Beef Stew</h1>
            <p>This hearty stew is my grandmother's cherished winter recipe, simmered for hours.</p>
            <p>Serve with crusty bread and a green salad for a complete meal.</p>
            <h2>Ingredients</h2>
            <ul>
              <li>2 lbs beef chuck</li>
              <li>3 carrots</li>
              <li>2 cups broth</li>
            </ul>
            <h2>Steps</h2>
            <ol>
              <li>Brown the beef.</li>
              <li>Simmer with vegetables.</li>
            </ol>
          </section>
        "#;
        let text = html_to_recipe_text(html, "Grandma's Beef Stew");
        let r = plain_recipe_from_text(&text, "Grandma's Beef Stew");
        assert!(
            is_cookable(&r),
            "headnoted recipe must remain cookable: ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert_eq!(r.ingredients.len(), 3);
        for ing in &r.ingredients {
            assert!(
                !ing.original.to_lowercase().contains("grandmother"),
                "headnote leaked into ingredients: {}",
                ing.original
            );
            assert!(
                !ing.original.to_lowercase().contains("crusty bread"),
                "headnote leaked into ingredients: {}",
                ing.original
            );
        }
        assert_eq!(r.steps.len(), 2);
    }

    #[test]
    fn long_headnote_does_not_drop_cookable_recipe() {
        // Nine narrative lines before Ingredients — previously diluted amt ratio.
        let mut lines = vec!["Tomato Soup".into(), "A quick weeknight soup.".into()];
        for i in 0..8 {
            lines.push(format!(
                "Family story paragraph number {i} about why we love this soup on cold days."
            ));
        }
        lines.push("Ingredients:".into());
        lines.push("2 cups tomatoes".into());
        lines.push("1 cup stock".into());
        lines.push("1 tbsp olive oil".into());
        lines.push("Steps:".into());
        lines.push("Simmer everything.".into());
        let text = lines.join("\n");
        let r = plain_recipe_from_text(&text, "Tomato Soup");
        assert!(
            is_cookable(&r),
            "long headnote must not drop recipe: ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert_eq!(r.ingredients.len(), 3);
        assert!(!r
            .ingredients
            .iter()
            .any(|i| i.original.contains("Family story")));
        assert!(!r
            .ingredients
            .iter()
            .any(|i| i.original.contains("weeknight")));
    }

    #[test]
    fn method_only_keeps_ingredients_strips_headnote() {
        // title → headnote → bulleted ingredients → Method: (no Ingredients:).
        // Must keep amount-bearing bullets and exclude leading headnote prose.
        let text = "\
Tomato Soup
A quick weeknight soup.
2 cups tomatoes
1 cup stock
1 tbsp olive oil
Method:
Simmer everything.
";
        let r = plain_recipe_from_text(text, "Tomato Soup");
        assert!(
            is_cookable(&r),
            "Method-only recipe must stay cookable: ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert_eq!(r.ingredients.len(), 3);
        assert!(
            !r.ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("weeknight")),
            "headnote leaked into ingredients: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert!(r
            .ingredients
            .iter()
            .any(|i| i.original.contains("tomatoes")));
        assert_eq!(r.steps.len(), 1);
        assert!(r.steps[0].contains("Simmer"));
    }

    #[test]
    fn directions_only_html_imports_ingredients() {
        // Reproduces the round-4 regression: no Ingredients heading.
        let html = r#"
          <section id="soup">
            <h1>Tomato Soup</h1>
            <ul>
              <li>2 cups tomatoes</li>
              <li>1 cup stock</li>
              <li>1 tbsp olive oil</li>
            </ul>
            <h2>Directions</h2>
            <ol>
              <li>Simmer everything.</li>
            </ol>
          </section>
        "#;
        let text = html_to_recipe_text(html, "Tomato Soup");
        let r = plain_recipe_from_text(&text, "Tomato Soup");
        assert!(
            is_cookable(&r),
            "Directions-only HTML must import: ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert_eq!(r.ingredients.len(), 3);
        assert_eq!(r.steps.len(), 1);
    }

    #[test]
    fn headerless_strips_leading_headnote_keeps_amounts() {
        let text = "\
Simple Salad
A bright summer side for picnics.
2 cups lettuce
1 cup tomatoes
1 tbsp oil
";
        let r = plain_recipe_from_text(text, "Simple Salad");
        assert!(
            is_cookable(&r),
            "headerless with headnote must stay cookable: ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert_eq!(r.ingredients.len(), 3);
        assert!(
            !r.ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("picnic")),
            "headnote leaked: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn element_lines_keeps_own_text_with_nested_blocks() {
        let html = r#"<li>2 cups flour<ul><li>preferably sifted</li></ul></li>"#;
        let lines = element_lines(html);
        assert!(
            lines.iter().any(|l| l.contains("2 cups flour")),
            "outer li own text lost: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("preferably sifted")),
            "nested li text lost: {lines:?}"
        );
    }

    #[test]
    fn element_lines_keeps_inline_markup_with_nested_blocks() {
        // Mixed inline + block: must not drop <i>sifted</i> when skipping nested <ul>.
        let html = r#"<li>2 cups <i>sifted</i> flour<ul><li>optional note</li></ul></li>"#;
        let lines = element_lines(html);
        let outer = lines
            .iter()
            .find(|l| l.contains("flour") || l.contains("sifted"))
            .cloned()
            .unwrap_or_default();
        assert!(
            outer.contains("sifted") && outer.contains("flour") && outer.contains("2 cups"),
            "inline text lost in mixed node: lines={lines:?}, outer={outer:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("optional note")),
            "nested li text lost: {lines:?}"
        );
    }

    #[test]
    fn segment_without_resolvable_fragments_emits_one() {
        let html = "<html><body><p>whole chapter</p></body></html>";
        let entries = vec![
            IndexEntry {
                title: "One".into(),
                path: "x".into(),
                fragment: Some("missing".into()),
                href: "#missing".into(),
            },
            IndexEntry {
                title: "Two".into(),
                path: "x".into(),
                fragment: Some("also-missing".into()),
                href: "#also-missing".into(),
            },
        ];
        let segs = segment_html(html, &entries);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].0.title, "One");
        assert!(segs[0].1.contains("whole chapter"));
    }

    #[test]
    fn epub_identity_keys_survive_normalize_url() {
        use crate::ingest::{epub_source_key, normalize_url};
        let a = epub_source_key("/tmp/book.epub", "recipes.xhtml#pancakes");
        let b = epub_source_key("/tmp/book.epub", "recipes.xhtml#omelette");
        assert_ne!(normalize_url(&a), normalize_url(&b));
        assert!(normalize_url(&a).contains("href="));
    }

    #[test]
    fn html_segment_parses_ingredients_and_steps() {
        let html = r#"
          <section id="pancakes">
            <h1>Fluffy Pancakes</h1>
            <h2>Ingredients</h2>
            <ul>
              <li>2 cups flour</li>
              <li>1 cup milk</li>
              <li>2 eggs</li>
            </ul>
            <h2>Steps</h2>
            <ol>
              <li>Mix dry and wet.</li>
              <li>Cook on griddle.</li>
            </ol>
          </section>
        "#;
        let text = html_to_recipe_text(html, "Fluffy Pancakes");
        let r = plain_recipe_from_text(&text, "Fluffy Pancakes");
        assert!(is_cookable(&r), "recipe: {:?}", r.ingredients);
        assert!(r.ingredients.len() >= 3);
        assert_eq!(r.steps.len(), 2);
    }

    #[test]
    fn full_epub_fixture_imports_two_recipes() {
        let dir = tempfile::tempdir().unwrap();
        let epub_path = dir.path().join("cookbook.epub");
        write_sample_epub(&epub_path);
        let recipes = ingest_epub(epub_path.to_str().unwrap()).expect("ingest");
        assert_eq!(
            recipes.len(),
            2,
            "got: {:?}",
            recipes.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        let titles: HashSet<_> = recipes.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.contains("Fluffy Pancakes"));
        assert!(titles.contains("Simple Omelette"));
        for r in &recipes {
            assert!(matches!(r.source, RecipeSource::Epub { .. }));
            assert!(is_cookable(r));
        }
    }

    #[test]
    fn store_keeps_both_epub_recipes_not_collapsed_by_dedup() {
        use crate::ingest::recipe_source_url;
        use crate::storage::Store;

        let dir = tempfile::tempdir().unwrap();
        let epub_path = dir.path().join("cookbook.epub");
        write_sample_epub(&epub_path);
        let recipes = ingest_epub(epub_path.to_str().unwrap()).expect("ingest");
        assert_eq!(recipes.len(), 2);

        let db = dir.path().join("t.db");
        let store = Store::open(&db).unwrap();
        let mut saved = 0;
        for r in &recipes {
            let src = recipe_source_url(r);
            assert!(
                !store.is_duplicate(src.as_deref()).unwrap(),
                "pre-save dup: {src:?}"
            );
            store.save_recipe(r).unwrap();
            saved += 1;
        }
        assert_eq!(saved, 2);
        assert_eq!(store.list_recipes(None).unwrap().len(), 2);

        // Re-import: both should now be duplicates.
        for r in &recipes {
            assert!(store.is_duplicate(recipe_source_url(r).as_deref()).unwrap());
        }
    }

    fn write_sample_epub(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let stored =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let deflated =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/epub+zip").unwrap();

        zip.start_file("META-INF/container.xml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/content.opf", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="uid" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:test:cookbook</dc:identifier>
    <dc:title>Test Cookbook</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="index" href="Text/index.xhtml" media-type="application/xhtml+xml"/>
    <item id="recipes" href="Text/recipes.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="nav"/>
    <itemref idref="index"/>
    <itemref idref="recipes"/>
  </spine>
</package>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/nav.xhtml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Nav</title></head>
<body>
  <nav epub:type="toc"><ol>
    <li><a href="Text/index.xhtml">Recipe Index</a></li>
    <li><a href="Text/recipes.xhtml">Recipes</a></li>
  </ol></nav>
</body></html>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/Text/index.xhtml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Recipe Index</title></head>
<body>
  <h1>Recipe Index</h1>
  <ul>
    <li><a href="recipes.xhtml#pancakes">Fluffy Pancakes</a></li>
    <li><a href="recipes.xhtml#omelette">Simple Omelette</a></li>
  </ul>
</body></html>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/Text/recipes.xhtml", deflated)
            .unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Recipes</title></head>
<body>
  <section id="pancakes">
    <h1>Fluffy Pancakes</h1>
    <h2>Ingredients</h2>
    <ul>
      <li>2 cups flour</li>
      <li>1 cup milk</li>
      <li>2 eggs</li>
    </ul>
    <h2>Steps</h2>
    <ol>
      <li>Mix dry and wet.</li>
      <li>Cook on griddle.</li>
    </ol>
  </section>
  <section id="omelette">
    <h1>Simple Omelette</h1>
    <h2>Ingredients</h2>
    <ul>
      <li>3 eggs</li>
      <li>1 tbsp butter</li>
      <li>1 pinch salt</li>
    </ul>
    <h2>Method</h2>
    <ol>
      <li>Beat eggs.</li>
      <li>Cook in butter.</li>
    </ol>
  </section>
</body></html>"#,
        )
        .unwrap();

        zip.finish().unwrap();
    }
}
