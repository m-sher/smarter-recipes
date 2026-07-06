//! EPUB cookbook ingestion via a linked recipe index (or TOC fallback).
//!
//! Pipeline: open book → find index (or TOC) → collect linked starts →
//! segment HTML by fragment/spine order → Ingredients/Steps heuristics →
//! [`Recipe`] values filtered by [`super::is_cookable`].

use super::{epub_source_key_for_resolved, is_cookable, resolve_epub_path};
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

/// Outcome of an EPUB batch ingest: cookable recipes plus titles skipped under
/// the never-guess ambiguous-structure policy.
#[derive(Debug, Clone, Default)]
pub struct EpubIngestOutcome {
    pub recipes: Vec<Recipe>,
    /// Titles not imported because ingredients could not be identified
    /// unambiguously from structure (no fabricated lists).
    pub skipped_ambiguous: Vec<String>,
}

/// Ingest all cookable recipes from an EPUB path.
pub fn ingest_epub(path: &str) -> Result<EpubIngestOutcome> {
    let doc = EpubDoc::new(path).with_context(|| format!("opening EPUB {path}"))?;
    ingest_epub_doc(doc, path)
}

fn ingest_epub_doc<R: Read + Seek>(mut doc: EpubDoc<R>, path: &str) -> Result<EpubIngestOutcome> {
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
    let mut skipped_ambiguous: Vec<String> = Vec::new();

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
            let (recipe, status) =
                html_segment_to_recipe(&entry, &segment_html, path, &resolved_epub_path);
            match status {
                IngredientStatus::AmbiguousStructure => {
                    skipped_ambiguous.push(recipe.title);
                }
                IngredientStatus::Resolved => {
                    if is_cookable(&recipe) {
                        out.push(recipe);
                    }
                }
            }
        }
    }

    if skipped_unresolved > 0 {
        eprintln!(
            "note: skipped {skipped_unresolved} EPUB index entr{} with unresolved fragment anchors",
            if skipped_unresolved == 1 { "y" } else { "ies" }
        );
    }
    if !skipped_ambiguous.is_empty() {
        eprintln!(
            "note: skipped {} recipe(s) with ambiguous ingredients \
             (need an Ingredients: heading, or list-only ingredients with no measured paragraphs; \
             enter manually if needed):",
            skipped_ambiguous.len()
        );
        for title in &skipped_ambiguous {
            eprintln!("  - {title}");
        }
    }

    if !loaded_any && missing_resources > 0 {
        bail!(
            "EPUB index/TOC pointed at {missing_resources} resource path(s) that could not be read"
        );
    }

    if out.is_empty() {
        if !skipped_ambiguous.is_empty() {
            bail!(
                "EPUB index/TOC produced entries but all had ambiguous or non-cookable ingredients \
                 ({} skipped as structurally ambiguous)",
                skipped_ambiguous.len()
            );
        }
        bail!(
            "EPUB index/TOC produced entries but none looked cookable \
             (need ≥2 ingredient lines with amounts)"
        );
    }
    Ok(EpubIngestOutcome {
        recipes: out,
        skipped_ambiguous,
    })
}

fn collect_entries<R: Read + Seek>(doc: &mut EpubDoc<R>) -> Result<Vec<IndexEntry>> {
    let mut candidates: Vec<(i64, Vec<IndexEntry>)> = Vec::new();

    // Score every HTML/XML spine resource as a potential catalog (Contents, Index, …).
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
        let parsed = parse_index_document(&html, &path_str);
        if let Some(score) = catalog_quality_score(&parsed, &path_str) {
            candidates.push((score, parsed.entries));
        }
    }

    // NCX / nav leaves are a first-class catalog (recipe titles, not page numbers).
    let toc_entries = entries_from_toc(doc);
    if toc_entries.len() >= 2 {
        let parsed = IndexParseResult {
            titled_link_count: toc_entries.len(),
            page_number_link_count: 0,
            entries: toc_entries,
        };
        if let Some(score) = catalog_quality_score(&parsed, "toc.ncx") {
            // Small bonus so a large nav catalog wins over a thin XHTML contents twin.
            candidates.push((score + 30, parsed.entries));
        }
    }

    pick_best_catalog(candidates).ok_or_else(|| {
        anyhow::anyhow!(
            "no linked recipe entries found in EPUB index or TOC; \
             need a recipe index with hyperlinks (page-number-only indexes unsupported)"
        )
    })
}

/// Link-scan result: titled recipe entries plus counts used for subject-index detection.
#[derive(Debug, Clone, Default)]
pub struct IndexParseResult {
    pub entries: Vec<IndexEntry>,
    /// Internal links whose text passed the titled-entry filters (same as `entries.len()`).
    pub titled_link_count: usize,
    /// Internal links rejected only because the label was a page/roman numeral.
    pub page_number_link_count: usize,
}

/// True when link text is only a page number or short roman numeral (`12`, `xiv`).
pub fn is_page_number_label(title: &str) -> bool {
    let title_lc = title.to_ascii_lowercase();
    !title_lc.is_empty()
        && title_lc.len() < 6
        && title_lc
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'x' | 'i' | 'v'))
}

/// Subject (back-matter) index: majority of labeled links are page numbers.
pub fn is_subject_index_stats(page_number_links: usize, titled_links: usize) -> bool {
    let total = page_number_links.saturating_add(titled_links);
    if total == 0 {
        return true;
    }
    // ≥ 70% page-number labels.
    page_number_links * 10 >= total * 7
}

/// Quality score for a catalog candidate. `None` = do not use (subject index / too small).
pub fn catalog_quality_score(parsed: &IndexParseResult, path_hint: &str) -> Option<i64> {
    if parsed.entries.len() < 2 {
        return None;
    }
    // Never treat a page-number subject index (plus a few section labels) as the catalog.
    if is_subject_index_stats(parsed.page_number_link_count, parsed.titled_link_count) {
        return None;
    }
    let mut score = parsed.entries.len() as i64 * 1000;
    // Prefer deep links (path#fragment) over top-level spine nav ("Recipes" → whole file).
    let with_fragment = parsed
        .entries
        .iter()
        .filter(|e| e.fragment.is_some())
        .count() as i64;
    score += with_fragment * 500;
    // Demote catalogs whose targets are mostly other index/nav/contents pages.
    let meta_targets = parsed
        .entries
        .iter()
        .filter(|e| path_looks_like_meta_catalog(&e.path))
        .count() as i64;
    score -= meta_targets * 800;
    score += catalog_path_bonus(path_hint) as i64;
    Some(score)
}

fn path_looks_like_meta_catalog(path: &str) -> bool {
    let lower = path.to_lowercase();
    let file = Path::new(&lower)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    matches!(
        file,
        "index"
            | "contents"
            | "toc"
            | "nav"
            | "table-of-contents"
            | "tableofcontents"
            | "cover"
            | "title"
            | "copyright"
    ) || file.contains("index") && !file.contains("recipe")
}

/// Path stem bonuses — tie-breakers only; titled count dominates via `catalog_quality_score`.
pub fn catalog_path_bonus(path: &str) -> i32 {
    let lower = path.to_lowercase();
    let file = Path::new(&lower)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(lower.as_str());
    if file == "recipe-index" || file == "recipe_index" || file == "recipes-index" {
        return 100;
    }
    if file.contains("recipe") && file.contains("index") {
        return 90;
    }
    if matches!(
        file,
        "contents" | "toc" | "table-of-contents" | "tableofcontents" | "nav"
    ) || file.ends_with("-contents")
        || file.ends_with("_contents")
    {
        return 60;
    }
    if file == "toc" || lower.contains("toc.ncx") || file.ends_with(".ncx") {
        return 55;
    }
    if file == "index" || file.ends_with("-index") || file.ends_with("_index") {
        // Mild bonus only when the document is *not* a subject index (caller filters those).
        return 20;
    }
    if file.contains("index") {
        return 10;
    }
    0
}

/// Pick the highest-scoring catalog; ties keep the first inserted.
pub fn pick_best_catalog(mut candidates: Vec<(i64, Vec<IndexEntry>)>) -> Option<Vec<IndexEntry>> {
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, e)| e)
}

/// Parse index/contents HTML: titled entries for import, plus page-number stats.
pub fn parse_index_document(html: &str, base_path: &str) -> IndexParseResult {
    let document = Html::parse_document(html);
    let Ok(sel) = Selector::parse("a[href]") else {
        return IndexParseResult::default();
    };
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut titled_link_count = 0usize;
    let mut page_number_link_count = 0usize;

    for a in document.select(&sel) {
        let href = a.value().attr("href").unwrap_or("").trim();
        if href.is_empty() || href.starts_with("http://") || href.starts_with("https://") {
            continue;
        }
        if href.starts_with("mailto:") || href.starts_with("javascript:") {
            continue;
        }
        // Skip stylesheet / non-document targets.
        let href_lower = href.to_ascii_lowercase();
        if href_lower.ends_with(".css")
            || href_lower.ends_with(".jpg")
            || href_lower.ends_with(".jpeg")
            || href_lower.ends_with(".png")
            || href_lower.ends_with(".gif")
            || href_lower.ends_with(".svg")
        {
            continue;
        }

        let title = sanitize(&a.text().collect::<String>());
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            continue;
        }
        if is_page_number_label(&title) {
            page_number_link_count += 1;
            continue;
        }
        if is_noise_title(&title) {
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
        titled_link_count += 1;
        entries.push(IndexEntry {
            title,
            path,
            fragment,
            href: href.to_string(),
        });
    }

    IndexParseResult {
        entries,
        titled_link_count,
        page_number_link_count,
    }
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

/// Parse `<a href>` entries from an index HTML document (titled links only).
#[cfg(test)]
pub fn parse_index_entries(html: &str, base_path: &str) -> Vec<IndexEntry> {
    parse_index_document(html, base_path).entries
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

/// How ingredients were obtained for a segment (no guessing on the no-heading path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngredientStatus {
    /// Explicit `Ingredients:` section, or unambiguous list-only structure.
    Resolved,
    /// No list structure, or list items coexisting with measured paragraphs.
    AmbiguousStructure,
}

fn html_segment_to_recipe(
    entry: &IndexEntry,
    segment_html: &str,
    epub_path: &str,
    resolved_epub_path: &str,
) -> (Recipe, IngredientStatus) {
    let (mut recipe, status) = recipe_from_html_status(segment_html, &entry.title);
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
    (recipe, status)
}

/// HTML element role for a source line — used to separate headnotes (`<p>`) from
/// ingredient bullets (`<li>`) without guessing from text shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Heading,
    Paragraph,
    /// `<li>`, `<dt>`/`<dd>`, or `.ingredient` nodes — preferred ingredient source.
    ListItem,
    /// No HTML structure (plain-text import / tag-strip fallback).
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceLine {
    kind: LineKind,
    text: String,
}

/// Build a recipe from an HTML fragment, preserving element kinds.
///
/// On the no-`Ingredients:` path, only **unambiguous list-only** structure is
/// promoted; ambiguous cases yield empty ingredients (see [`IngredientStatus`]).
fn recipe_from_html_status(html: &str, fallback_title: &str) -> (Recipe, IngredientStatus) {
    recipe_from_source_lines(&html_to_source_lines(html, fallback_title), fallback_title)
}

#[cfg(test)]
fn recipe_from_html(html: &str, fallback_title: &str) -> Recipe {
    recipe_from_html_status(html, fallback_title).0
}

fn html_to_source_lines(html: &str, fallback_title: &str) -> Vec<SourceLine> {
    let body = element_lines(html);
    let mut out = Vec::new();
    out.push(SourceLine {
        kind: LineKind::Heading,
        text: fallback_title.to_string(),
    });
    let mut section = "body";
    for line in body {
        let lower = line.text.to_lowercase();
        if is_ingredients_header(&lower) {
            out.push(SourceLine {
                kind: LineKind::Heading,
                text: "Ingredients:".into(),
            });
            section = "ingredients";
            continue;
        }
        if is_steps_header(&lower) {
            out.push(SourceLine {
                kind: LineKind::Heading,
                text: "Steps:".into(),
            });
            section = "steps";
            continue;
        }
        // Skip repeating the title line (common when the heading is also in body text).
        if section == "body"
            && normalize_title_key(&line.text) == normalize_title_key(fallback_title)
        {
            continue;
        }
        out.push(line);
    }
    out
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

/// Extract visible text lines from HTML with their source element kind.
fn element_lines(html: &str) -> Vec<SourceLine> {
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
            let kind = line_kind_for_element(el);
            lines.push(SourceLine { kind, text: t });
        }
    }
    if lines.is_empty() {
        // Fallback: all text collapsed by lines from raw strip (no structure).
        let stripped = strip_tags(html);
        for line in stripped.lines() {
            let t = line.trim();
            if !t.is_empty() {
                lines.push(SourceLine {
                    kind: LineKind::Plain,
                    text: sanitize(t),
                });
            }
        }
    }
    lines
}

fn line_kind_for_element(el: ElementRef<'_>) -> LineKind {
    let name = el.value().name();
    let class_attr = el.value().attr("class").unwrap_or("");
    // Publisher CSS ingredient blocks (e.g. Agate `p.hang`) are structural, like `<li>`.
    if is_ingredient_class_attr(class_attr) {
        return LineKind::ListItem;
    }
    match name {
        "h1" | "h2" | "h3" | "h4" => LineKind::Heading,
        "li" | "dt" | "dd" => LineKind::ListItem,
        "p" => LineKind::Paragraph,
        "div" | "span" => {
            // Only matched via `.ingredient` in the selector (also covered by class check).
            LineKind::ListItem
        }
        _ => LineKind::Paragraph,
    }
}

/// Whole-token class match for ingredient-paragraph conventions (never free-text guess).
pub fn is_ingredient_class_attr(class_attr: &str) -> bool {
    class_attr.split_whitespace().any(|tok| {
        matches!(
            tok.to_ascii_lowercase().as_str(),
            "hang" | "ingredient" | "ingredients" | "ing" | "recipe-ingredient"
        )
    })
}

/// Nutrition / exchanges section markers that end the ingredient block.
pub fn is_nutrition_block_header(line: &str) -> bool {
    let t = line.trim();
    let lower = t.to_ascii_lowercase();
    lower.starts_with("per serving")
        || lower.starts_with("calories:")
        || lower.starts_with("calories ")
        || lower == "calories"
        || lower.starts_with("exchanges:")
        || lower == "exchanges"
        || lower.starts_with("% of calories from fat")
        || lower.starts_with("saturated fat")
        || lower.starts_with("cholesterol")
        || lower.starts_with("sodium:")
        || lower.starts_with("protein:")
        || lower.starts_with("carbohydrate:")
}

/// Numbered procedure line (`1. Mix…`, `2) Bake…`).
pub fn is_numbered_step_line(line: &str) -> bool {
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d+[\.)]\s+\S").expect("numbered step re"));
    RE.is_match(line.trim())
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

#[allow(dead_code)] // retained for future density scoring of catalog pages
fn html_visible_len(html: &str) -> usize {
    strip_tags(html).split_whitespace().map(|w| w.len()).sum()
}

/// Same structure as file plain-text import (no HTML element kinds).
///
/// Without list structure, the no-`Ingredients:` path is **ambiguous** (no
/// guessing). Prefer [`recipe_from_html`] when source HTML is available.
#[cfg(test)]
fn plain_recipe_from_text(text: &str, title_override: &str) -> Recipe {
    let mut lines = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let kind = if lines.is_empty() {
            LineKind::Heading
        } else {
            LineKind::Plain
        };
        lines.push(SourceLine {
            kind,
            text: t.to_string(),
        });
    }
    recipe_from_source_lines(&lines, title_override).0
}

fn recipe_from_source_lines(
    lines: &[SourceLine],
    title_override: &str,
) -> (Recipe, IngredientStatus) {
    let mut title = title_override.to_string();
    let mut ingredients = Vec::new();
    let mut steps = Vec::new();
    let mut section = "title";
    let mut saw_ingredients_header = false;
    // Buffered body before Ingredients/Steps; promoted only when unambiguous.
    let mut pre_header_body: Vec<SourceLine> = Vec::new();

    for line in lines {
        let t = line.text.trim();
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
        // Nutrition block ends pre-header / class-based ingredients (not an ingredient source).
        if matches!(section, "body" | "title") && is_nutrition_block_header(t) {
            section = "post_ingredients";
            continue;
        }
        // Numbered procedure lines switch into steps without a Method: header.
        if matches!(section, "body" | "post_ingredients") && is_numbered_step_line(t) {
            section = "steps";
            // fall through to steps arm
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
            "post_ingredients" => {
                // Skip nutrition / exchanges lines until a numbered step or steps header.
            }
            _ => {
                // Headnote paragraphs / ingredient list items before headers.
                pre_header_body.push(SourceLine {
                    kind: line.kind,
                    text: t.trim_start_matches(['-', '*', '•']).to_string(),
                });
            }
        }
    }

    let mut status = IngredientStatus::Resolved;
    // No Ingredients: section → promote **only** when structure is unambiguous.
    // Never invent ingredients from text shape or mixed list+measured-paragraph markup.
    if !saw_ingredients_header && ingredients.is_empty() && !pre_header_body.is_empty() {
        match resolve_pre_header_ingredients(&pre_header_body) {
            PreHeaderIngredients::ListItems(items) => {
                ingredients = items.iter().map(|s| normalize_line(s)).collect();
            }
            PreHeaderIngredients::Ambiguous => {
                status = IngredientStatus::AmbiguousStructure;
            }
        }
    }

    let mut recipe = Recipe::new(sanitize(&title));
    recipe.ingredients = ingredients;
    recipe.steps = steps;
    (recipe, status)
}

/// Unambiguous list-only promote, or skip (no guessing).
enum PreHeaderIngredients {
    /// List items only; no non-heading paragraph carries unit/quantity.
    ListItems(Vec<String>),
    /// No list items, or list coexists with a measured paragraph/plain line.
    Ambiguous,
}

/// On the no-`Ingredients:` path: import list items only when that is the sole
/// structural ingredient source. Never guess from prose or merge mixed markup.
fn resolve_pre_header_ingredients(lines: &[SourceLine]) -> PreHeaderIngredients {
    let list_items: Vec<String> = lines
        .iter()
        .filter(|l| l.kind == LineKind::ListItem)
        .map(|l| l.text.clone())
        .collect();
    let has_measured_paragraph = lines.iter().any(|l| {
        matches!(l.kind, LineKind::Paragraph | LineKind::Plain)
            && looks_like_measured_ingredient(&l.text)
    });

    if list_items.is_empty() {
        // Would require text-shape guessing — refuse.
        return PreHeaderIngredients::Ambiguous;
    }
    if has_measured_paragraph {
        // List tip coexisting with `<p>2 cups flour</p>`-style lines — refuse.
        return PreHeaderIngredients::Ambiguous;
    }
    // Headnote paragraphs without measure/quantity are fine; list items win.
    PreHeaderIngredients::ListItems(list_items)
}

fn looks_like_measured_ingredient(line: &str) -> bool {
    // Yield lines co-exist with real ingredient lists; they are not competing `<p>` ingredients.
    if looks_like_yield_line(line) {
        return false;
    }
    has_measure_unit_token(line) || starts_with_quantity_token(line)
}

/// `16 servings`, `Serves 4`, `Makes about 2 cups` — not an ingredient paragraph.
pub fn looks_like_yield_line(line: &str) -> bool {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(?:\d[\d./\-–to\s]*\s+servings?\b|serves\b|makes\b|yield(?:s|ed)?\b|about\s+\d+\s+servings?\b)",
        )
        .expect("yield re")
    });
    RE.is_match(line.trim())
}

/// Common culinary unit / measure tokens (word-boundary). Bare numbers do not count.
fn has_measure_unit_token(line: &str) -> bool {
    static UNIT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)\b(?:cups?|tbsps?|tsps?|teaspoons?|tablespoons?|lbs?|pounds?|oz|ounces?|kg|kilograms?|g|grams?|ml|milliliters?|l|liters?|litres?|pints?|quarts?|gallons?|cloves?|pinches?|dashes?|cans?|packages?|pkg|sticks?|bunches?|heads?|slices?|pieces?|sprigs?|handfuls?|scoops?|drops?)\b",
        )
        .expect("unit re")
    });
    UNIT.is_match(line)
}

fn starts_with_quantity_token(line: &str) -> bool {
    let t = line.trim_start();
    let Some(first) = t.chars().next() else {
        return false;
    };
    first.is_ascii_digit()
        || matches!(
            first,
            '½' | '¼'
                | '¾'
                | '⅓'
                | '⅔'
                | '⅕'
                | '⅖'
                | '⅗'
                | '⅘'
                | '⅙'
                | '⅚'
                | '⅛'
                | '⅜'
                | '⅝'
                | '⅞'
        )
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
        let parsed = parse_index_document(html, "OEBPS/Text/index.xhtml");
        assert_eq!(parsed.titled_link_count, 2);
        assert_eq!(parsed.page_number_link_count, 1);
    }

    #[test]
    fn page_number_label_detection() {
        assert!(is_page_number_label("12"));
        assert!(is_page_number_label("xiv"));
        assert!(is_page_number_label("XII"));
        assert!(!is_page_number_label("Baked Artichoke Dip"));
        assert!(!is_page_number_label("Chapter 12"));
    }

    #[test]
    fn subject_index_stats_majority_page_numbers() {
        assert!(is_subject_index_stats(20, 3));
        assert!(!is_subject_index_stats(2, 10));
        assert!(is_subject_index_stats(0, 0));
    }

    #[test]
    fn catalog_prefers_contents_titled_links_over_subject_index() {
        let subject = r#"<html><body>
          <a href="Chapter001sec1.html#page_2">2</a>
          <a href="Chapter001sec2.html#page_3">3</a>
          <a href="Chapter001sec3.html#page_4">4</a>
          <a href="Chapter001sec4.html#page_5">5</a>
          <a href="Chapter001sec5.html#page_6">6</a>
          <a href="Chapter001sec6.html#page_7">7</a>
          <a href="Chapter001sec7.html#page_8">8</a>
          <a href="Index.html#ind2">Casseroles</a>
          <a href="Index.html#ind3">Beans and Legumes</a>
          <a href="Index.html#ind4">Beef</a>
        </body></html>"#;
        let contents = r#"<html><body>
          <a href="Chapter001sec1.html#sec1">Baked Artichoke Dip</a>
          <a href="Chapter001sec2.html#sec2">Curry Dip</a>
          <a href="Chapter001sec3.html#sec3">Toasted Onion Dip</a>
          <a href="Chapter001sec4.html#sec4">Sun-Dried Tomato Hummus</a>
          <a href="Chapter001sec5.html#sec5">Black Bean Hummus</a>
          <a href="Chapter001sec6.html#sec6">Roasted Garlic Dip</a>
          <a href="Chapter001sec7.html#sec7">Black Bean Dip</a>
          <a href="Chapter001sec8.html#sec8">Pinto Bean Dip</a>
          <a href="Chapter001sec9.html#sec9">Chili Con Queso</a>
          <a href="Chapter001sec10.html#sec10">Queso Fundido</a>
        </body></html>"#;
        let sub = parse_index_document(subject, "OEBPS/Index.html");
        let con = parse_index_document(contents, "OEBPS/Contents.html");
        assert!(
            catalog_quality_score(&sub, "OEBPS/Index.html").is_none(),
            "subject index must not be a catalog candidate"
        );
        let con_score =
            catalog_quality_score(&con, "OEBPS/Contents.html").expect("contents should score");
        assert!(con_score > 0);
        assert_eq!(con.entries.len(), 10);
        let best = pick_best_catalog(vec![
            // Even if something wrongly scored the subject entries alone:
            (con_score, con.entries.clone()),
        ])
        .unwrap();
        assert_eq!(best.len(), 10);
        assert!(best.iter().any(|e| e.title.contains("Artichoke")));
        assert!(!best.iter().any(|e| e.title == "Beef"));
    }

    #[test]
    fn subject_index_only_is_not_a_usable_catalog() {
        let subject = r#"<html><body>
          <a href="a.html#p1">1</a><a href="a.html#p2">2</a><a href="a.html#p3">3</a>
          <a href="a.html#p4">4</a><a href="a.html#p5">5</a><a href="a.html#p6">6</a>
          <a href="a.html#p7">7</a><a href="a.html#p8">8</a><a href="a.html#p9">9</a>
          <a href="a.html#p10">10</a>
          <a href="Index.html#x">Pies</a>
          <a href="Index.html#y">Cakes</a>
        </body></html>"#;
        let parsed = parse_index_document(subject, "OEBPS/Index.html");
        assert_eq!(parsed.entries.len(), 2);
        assert!(catalog_quality_score(&parsed, "OEBPS/Index.html").is_none());
        assert!(pick_best_catalog(vec![]).is_none());
    }

    #[test]
    fn hang_class_paragraphs_are_ingredients_not_headnote_or_nutrition() {
        let html = r#"
        <html><body>
          <p class="subheadingr">BAKED ARTICHOKE DIP</p>
          <p class="normalr"><i>Everyone's favorite, modified for healthful, low-fat goodness.</i></p>
          <p class="hangms"><b>16 servings</b> (about 3 tablespoons each)</p>
          <p class="hang">1 can (15 ounces) artichoke hearts, rinsed, drained</p>
          <p class="hang">4 ounces fat-free cream cheese, room temperature</p>
          <p class="hang">1/2 cup fat-free mayonnaise</p>
          <p class="hang">2 teaspoons minced garlic</p>
          <p class="hangss"><b>Per Serving:</b></p>
          <p class="hangsr">Calories: 39</p>
          <p class="hangsr">Protein (gm): 3.5</p>
          <p class="normal-ts1"><b>1.</b> Process artichoke hearts and cream cheese until smooth.</p>
          <p class="normal-ts1"><b>2.</b> Bake until hot.</p>
        </body></html>
        "#;
        let (recipe, status) = recipe_from_html_status(html, "Baked Artichoke Dip");
        assert_eq!(status, IngredientStatus::Resolved);
        assert!(
            recipe.ingredients.len() >= 4,
            "ingredients={:?}",
            recipe.ingredients
        );
        assert!(
            !recipe
                .ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("everyone")),
            "headnote must not be an ingredient: {:?}",
            recipe.ingredients
        );
        assert!(
            !recipe
                .ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("calories")),
            "nutrition must not be ingredients: {:?}",
            recipe.ingredients
        );
        assert!(
            !recipe
                .ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("16 servings")),
            "yield line (hangms) must not be an ingredient"
        );
        assert!(
            !recipe.steps.is_empty(),
            "expected numbered steps, got {:?}",
            recipe.steps
        );
        assert!(is_cookable(&recipe), "recipe should be cookable");
    }

    #[test]
    fn measured_paragraphs_without_ingredient_class_still_ambiguous() {
        let html = r#"
        <html><body>
          <h1>Dressing</h1>
          <p>A zippy little sauce we adore.</p>
          <p>2 cups yogurt</p>
          <p>1 clove garlic</p>
          <h2>Method</h2>
          <p>Whisk together.</p>
        </body></html>
        "#;
        let (_recipe, status) = recipe_from_html_status(html, "Dressing");
        assert_eq!(status, IngredientStatus::AmbiguousStructure);
    }

    #[test]
    fn ingredient_class_attr_token_match() {
        assert!(is_ingredient_class_attr("hang"));
        assert!(is_ingredient_class_attr("foo hang bar"));
        assert!(!is_ingredient_class_attr("hangms"));
        assert!(!is_ingredient_class_attr("hangsr"));
        assert!(is_ingredient_class_attr("recipe-ingredient"));
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
        let r = recipe_from_html(html, "Grandma's Beef Stew");
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
        // Structured HTML: paragraph headnote + list ingredients + Method.
        let html = r#"
          <h1>Tomato Soup</h1>
          <p>A quick weeknight soup.</p>
          <ul>
            <li>2 cups tomatoes</li>
            <li>1 cup stock</li>
            <li>1 tbsp olive oil</li>
          </ul>
          <h2>Method</h2>
          <ol><li>Simmer everything.</li></ol>
        "#;
        let r = recipe_from_html(html, "Tomato Soup");
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
        let r = recipe_from_html(html, "Tomato Soup");
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
    fn list_item_ingredient_with_period_kept() {
        // Round-6: unquantified `<li>….</li>` must not be dropped as "sentence headnote".
        let html = r#"
          <h1>Steak</h1>
          <p>An old family favorite</p>
          <ul>
            <li>Freshly ground black pepper.</li>
            <li>2 lb ribeye</li>
            <li>1 tbsp oil</li>
          </ul>
          <h2>Directions</h2>
          <ol><li>Sear hard.</li></ol>
        "#;
        let r = recipe_from_html(html, "Steak");
        assert!(
            is_cookable(&r),
            "ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert!(
            r.ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("black pepper")),
            "period-terminated list ingredient dropped: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert!(
            !r.ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("family favorite")),
            "paragraph headnote leaked: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn short_paragraph_headnote_without_punct_stripped() {
        // Round-6: `<p>An old family favorite</p>` has no sentence punct but is still headnote.
        let html = r#"
          <h1>Stew</h1>
          <p>An old family favorite</p>
          <ul>
            <li>2 cups broth</li>
            <li>1 onion</li>
            <li>1 lb beef</li>
          </ul>
          <h2>Method</h2>
          <ol><li>Simmer.</li></ol>
        "#;
        let r = recipe_from_html(html, "Stew");
        assert!(is_cookable(&r));
        assert_eq!(r.ingredients.len(), 3);
        assert!(
            !r.ingredients.iter().any(|i| {
                let o = i.original.to_lowercase();
                o.contains("family") || o.contains("favorite")
            }),
            "short paragraph headnote leaked: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mixed_list_tip_and_paragraph_ingredients_is_ambiguous() {
        // Owner bar: never guess. List tip + measured `<p>` ingredients = skip.
        let html = r#"
          <h1>Pancakes</h1>
          <p>A weekend treat with a tip:</p>
          <ul><li>Use fresh berries if you can</li></ul>
          <p>2 cups flour</p>
          <p>1 egg</p>
          <p>1 cup milk</p>
          <h2>Method</h2>
          <ol><li>Mix and griddle.</li></ol>
        "#;
        let (r, status) = recipe_from_html_status(html, "Pancakes");
        assert_eq!(status, IngredientStatus::AmbiguousStructure);
        assert!(
            r.ingredients.is_empty(),
            "must not fabricate/partial ingredients: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert!(!is_cookable(&r));
    }

    #[test]
    fn ingest_reports_ambiguous_skip_by_title() {
        // End-to-end: book with one clean list recipe + one ambiguous mixed segment.
        let dir = tempfile::tempdir().unwrap();
        let epub_path = dir.path().join("mixed.epub");
        write_ambiguous_mix_epub(&epub_path);
        let outcome = ingest_epub(epub_path.to_str().unwrap()).expect("ingest");
        // Only the unambiguous Method-style list recipe should import.
        assert_eq!(
            outcome.recipes.len(),
            1,
            "got: {:?}",
            outcome.recipes.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert_eq!(outcome.recipes[0].title, "Tomato Soup");
        assert!(is_cookable(&outcome.recipes[0]));
        assert_eq!(outcome.recipes[0].ingredients.len(), 3);
        assert_eq!(outcome.skipped_ambiguous, vec!["Pancakes".to_string()]);
    }

    #[test]
    fn all_paragraph_no_list_is_ambiguous() {
        // No list structure and no Ingredients: heading → refuse to text-guess.
        let html = r#"
          <h1>Simple Salad</h1>
          <p>A bright summer side for picnics.</p>
          <p>2 cups lettuce</p>
          <p>1 cup tomatoes</p>
          <p>1 tbsp oil</p>
        "#;
        let (r, status) = recipe_from_html_status(html, "Simple Salad");
        assert_eq!(status, IngredientStatus::AmbiguousStructure);
        assert!(r.ingredients.is_empty());
        assert!(!is_cookable(&r));
    }

    #[test]
    fn list_only_with_prose_headnote_imports() {
        // Unambiguous: headnote `<p>` (no measure) + ingredient `<li>` only.
        let html = r#"
          <h1>Simple Salad</h1>
          <p>A bright summer side for picnics.</p>
          <ul>
            <li>2 cups lettuce</li>
            <li>1 cup tomatoes</li>
            <li>1 tbsp oil</li>
          </ul>
          <h2>Directions</h2>
          <ol><li>Toss.</li></ol>
        "#;
        let (r, status) = recipe_from_html_status(html, "Simple Salad");
        assert_eq!(status, IngredientStatus::Resolved);
        assert!(is_cookable(&r));
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
    fn numeric_headnote_sentence_not_kept_as_ingredient() {
        // Paragraph headnote with a digit must not become an ingredient when lists exist.
        let html = r#"
          <h1>Roast Chicken</h1>
          <p>Serves 4 as a hearty Sunday dinner.</p>
          <ul>
            <li>2 lb chicken</li>
            <li>1 tbsp salt</li>
          </ul>
          <h2>Method</h2>
          <ol><li>Roast until done.</li></ol>
        "#;
        let r = recipe_from_html(html, "Roast Chicken");
        assert!(
            is_cookable(&r),
            "expected cookable: ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert_eq!(r.ingredients.len(), 2);
        assert!(
            !r.ingredients.iter().any(|i| {
                let o = i.original.to_lowercase();
                o.contains("serves") || o.contains("sunday")
            }),
            "numeric headnote leaked: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert!(r.ingredients.iter().any(|i| i.original.contains("chicken")));
        assert!(r.ingredients.iter().any(|i| i.original.contains("salt")));
    }

    #[test]
    fn leading_unquantified_ingredient_kept_before_amounts() {
        // List-item "Salt and pepper to taste" kept; paragraph headnote dropped.
        let html = r#"
          <h1>Guacamole</h1>
          <p>A party favorite from our summer cookouts.</p>
          <ul>
            <li>Salt and pepper to taste</li>
            <li>3 avocados</li>
            <li>1 lime</li>
          </ul>
          <h2>Directions</h2>
          <ol><li>Mash and mix.</li></ol>
        "#;
        let r = recipe_from_html(html, "Guacamole");
        assert!(
            is_cookable(&r),
            "expected cookable: ings={:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert!(
            r.ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("salt")
                    && i.original.to_lowercase().contains("pepper")),
            "unquantified leading ingredient dropped: {:?}",
            r.ingredients
                .iter()
                .map(|i| &i.original)
                .collect::<Vec<_>>()
        );
        assert!(r
            .ingredients
            .iter()
            .any(|i| i.original.contains("avocados")));
        assert!(r.ingredients.iter().any(|i| i.original.contains("lime")));
        assert!(
            !r.ingredients
                .iter()
                .any(|i| i.original.to_lowercase().contains("cookout")),
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
            lines.iter().any(|l| l.text.contains("2 cups flour")),
            "outer li own text lost: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.text.contains("preferably sifted")),
            "nested li text lost: {lines:?}"
        );
        assert!(lines.iter().all(|l| l.kind == LineKind::ListItem));
    }

    #[test]
    fn element_lines_keeps_inline_markup_with_nested_blocks() {
        // Mixed inline + block: must not drop <i>sifted</i> when skipping nested <ul>.
        let html = r#"<li>2 cups <i>sifted</i> flour<ul><li>optional note</li></ul></li>"#;
        let lines = element_lines(html);
        let outer = lines
            .iter()
            .find(|l| l.text.contains("flour") || l.text.contains("sifted"))
            .map(|l| l.text.clone())
            .unwrap_or_default();
        assert!(
            outer.contains("sifted") && outer.contains("flour") && outer.contains("2 cups"),
            "inline text lost in mixed node: lines={lines:?}, outer={outer:?}"
        );
        assert!(
            lines.iter().any(|l| l.text.contains("optional note")),
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
        let r = recipe_from_html(html, "Fluffy Pancakes");
        assert!(is_cookable(&r), "recipe: {:?}", r.ingredients);
        assert!(r.ingredients.len() >= 3);
        assert_eq!(r.steps.len(), 2);
    }

    #[test]
    fn full_epub_fixture_imports_two_recipes() {
        let dir = tempfile::tempdir().unwrap();
        let epub_path = dir.path().join("cookbook.epub");
        write_sample_epub(&epub_path);
        let outcome = ingest_epub(epub_path.to_str().unwrap()).expect("ingest");
        assert!(outcome.skipped_ambiguous.is_empty());
        assert_eq!(
            outcome.recipes.len(),
            2,
            "got: {:?}",
            outcome.recipes.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        let titles: HashSet<_> = outcome.recipes.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.contains("Fluffy Pancakes"));
        assert!(titles.contains("Simple Omelette"));
        for r in &outcome.recipes {
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
        let recipes = ingest_epub(epub_path.to_str().unwrap())
            .expect("ingest")
            .recipes;
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

    /// One unambiguous list+Directions recipe and one mixed tip-li + measured-p recipe.
    fn write_ambiguous_mix_epub(path: &Path) {
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
    <dc:identifier id="uid">urn:test:mixed</dc:identifier>
    <dc:title>Mixed Cookbook</dc:title>
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
    <li><a href="recipes.xhtml#soup">Tomato Soup</a></li>
    <li><a href="recipes.xhtml#pancakes">Pancakes</a></li>
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
  <section id="pancakes">
    <h1>Pancakes</h1>
    <p>A weekend treat with a tip:</p>
    <ul><li>Use fresh berries if you can</li></ul>
    <p>2 cups flour</p>
    <p>1 egg</p>
    <p>1 cup milk</p>
    <h2>Method</h2>
    <ol><li>Mix and griddle.</li></ol>
  </section>
</body></html>"#,
        )
        .unwrap();

        zip.finish().unwrap();
    }
}
