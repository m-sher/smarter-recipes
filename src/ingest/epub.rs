//! EPUB cookbook ingestion via a linked recipe index (or TOC fallback).
//!
//! Pipeline: open book → find index (or TOC) → collect linked starts →
//! segment HTML by fragment/spine order → Ingredients/Steps heuristics →
//! [`Recipe`] values filtered by [`super::is_cookable`].

use super::is_cookable;
use crate::domain::{Recipe, RecipeMeta, RecipeSource};
use crate::normalize::normalize_line;
use crate::text::sanitize;
use anyhow::{bail, Context, Result};
use epub::doc::EpubDoc;
use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{Html, Selector};
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

    let spine_order = spine_path_order(&doc);
    let mut by_path: HashMap<String, Vec<IndexEntry>> = HashMap::new();
    for e in entries {
        by_path.entry(e.path.clone()).or_default().push(e);
    }

    // Process resources in spine order; within a file, fragment order.
    let mut path_keys: Vec<String> = by_path.keys().cloned().collect();
    path_keys.sort_by_key(|p| spine_order.get(p).copied().unwrap_or(usize::MAX));

    let mut out = Vec::new();
    let mut seen_titles: HashSet<String> = HashSet::new();

    for res_path in path_keys {
        let mut group = by_path.remove(&res_path).unwrap_or_default();
        let html = match doc.get_resource_str_by_path(Path::new(&res_path)) {
            Some(s) => s,
            None => continue,
        };
        // Order entries by first occurrence of fragment in HTML.
        group.sort_by_key(|e| fragment_offset(&html, e.fragment.as_deref()).unwrap_or(usize::MAX));

        let segments = segment_html(&html, &group);
        for (entry, segment_html) in segments {
            let title_key = normalize_title_key(&entry.title);
            if title_key.is_empty() || !seen_titles.insert(title_key) {
                continue;
            }
            let recipe = html_segment_to_recipe(&entry, &segment_html, path);
            if is_cookable(&recipe) {
                out.push(recipe);
            }
        }
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

fn find_index_path<R: Read + Seek>(doc: &EpubDoc<R>) -> Option<String> {
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
    // Also check TOC leaves labeled like an index.
    for np in flatten_nav(&doc.toc) {
        let label = np.label.to_lowercase();
        if label.contains("recipe index") || label == "index" || label.contains("index of recipes")
        {
            return Some(path_to_string(&np.content));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
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
        let score = entries.len().saturating_mul(1000).saturating_sub(text_len / 20);
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
        // Skip pure page-number labels ("12", "xiv").
        if title.chars().all(|c| c.is_ascii_digit() || c == 'x' || c == 'i' || c == 'v')
            && title.len() < 6
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
    static NOISE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(cover|title page|copyright|contents|table of contents|toc|acknowledg|\
foreword|preface|introduction|about the author|bibliography|glossary|notes?|\
dedication|half title|also by|colophon)s?$",
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
        Some((p, f)) => (p, if f.is_empty() { None } else { Some(f.to_string()) }),
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

fn fragment_offset(html: &str, fragment: Option<&str>) -> Option<usize> {
    let frag = fragment?;
    // id="frag" or name="frag" or id='frag'
    let patterns = [
        format!("id=\"{frag}\""),
        format!("id='{frag}'"),
        format!("name=\"{frag}\""),
        format!("name='{frag}'"),
    ];
    patterns.iter().filter_map(|p| html.find(p)).min()
}

/// Split HTML into per-entry segments using fragment anchors.
///
/// Entries without a fragment that share a file alone get the whole body.
/// Multiple fragment-less entries on one file cannot be split — each gets the full body
/// (later cookable/title dedup filters noise).
pub fn segment_html(html: &str, entries: &[IndexEntry]) -> Vec<(IndexEntry, String)> {
    if entries.is_empty() {
        return Vec::new();
    }
    if entries.len() == 1 {
        return vec![(entries[0].clone(), html.to_string())];
    }

    // Build cut points: (byte offset, entry index). Missing fragments → offset 0.
    let mut cuts: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let off = fragment_offset(html, e.fragment.as_deref()).unwrap_or(0);
            (off, i)
        })
        .collect();
    cuts.sort_by_key(|(off, i)| (*off, *i));

    let mut out = Vec::new();
    for (pos, &(start, entry_i)) in cuts.iter().enumerate() {
        let end = cuts
            .get(pos + 1)
            .map(|(next, _)| *next)
            .unwrap_or(html.len());
        // If start==end and not last, skip empty; if all share offset 0, still give full html to each
        // only when no fragments were found at all.
        let slice = if start < end {
            html[start..end].to_string()
        } else if start == 0 && end == 0 {
            // degenerate
            String::new()
        } else {
            String::new()
        };
        // When no fragments resolve, every entry would get empty or same — give full doc once-ish.
        let slice = if slice.is_empty() && entries[entry_i].fragment.is_none() {
            html.to_string()
        } else {
            slice
        };
        out.push((entries[entry_i].clone(), slice));
    }
    out
}

fn html_segment_to_recipe(entry: &IndexEntry, segment_html: &str, epub_path: &str) -> Recipe {
    let text = html_to_recipe_text(segment_html, &entry.title);
    let mut recipe = plain_recipe_from_text(&text, &entry.title);
    recipe.source = RecipeSource::Epub {
        path: epub_path.to_string(),
        href: entry.href.clone(),
    };
    recipe.meta = RecipeMeta {
        notes: Some(format!("Imported from EPUB index entry: {}", entry.href)),
        source_url: Some(format!("epub://{}#{}", epub_path, entry.href)),
        ..Default::default()
    };
    recipe
}

/// Convert a recipe HTML fragment into plain text with Ingredients:/Steps: markers.
pub fn html_to_recipe_text(html: &str, fallback_title: &str) -> String {
    let document = Html::parse_fragment(html);
    let mut title = fallback_title.to_string();
    if let Ok(h_sel) = Selector::parse("h1, h2, h3") {
        if let Some(h) = document.select(&h_sel).next() {
            let t = sanitize(&h.text().collect::<String>());
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() && !is_section_header(&t) {
                title = t;
            }
        }
    }

    let body_text = element_lines(html);
    let mut out = Vec::new();
    out.push(title);
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
        // Skip repeating the title line.
        if section == "body" && normalize_title_key(&line) == normalize_title_key(fallback_title) {
            continue;
        }
        if section == "body" {
            // Before any header: treat non-header lines as potential ingredients later via body mode.
            out.push(line);
        } else {
            out.push(line);
        }
    }
    out.join("\n")
}

fn is_section_header(t: &str) -> bool {
    let l = t.to_lowercase();
    is_ingredients_header(&l) || is_steps_header(&l)
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
    if let Ok(sel) = Selector::parse("h1, h2, h3, h4, p, li, dt, dd, div.ingredient, span.ingredient")
    {
        for el in document.select(&sel) {
            // Skip empty / whitespace-only.
            let t = sanitize(&el.text().collect::<String>());
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.is_empty() {
                continue;
            }
            // Avoid mega nested duplicates: skip if this element has block children we also select.
            let has_block_child = el
                .children()
                .filter_map(|n| n.value().as_element())
                .any(|e| {
                    matches!(
                        e.name(),
                        "h1" | "h2" | "h3" | "h4" | "p" | "li" | "ul" | "ol" | "div"
                    )
                });
            if has_block_child {
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
pub fn plain_recipe_from_text(text: &str, title_override: &str) -> Recipe {
    let mut title = title_override.to_string();
    let mut ingredients = Vec::new();
    let mut steps = Vec::new();
    let mut section = "title";

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_lowercase();
        if is_ingredients_header(&lower) || (lower.starts_with("ingredients") && t.contains(':')) {
            section = "ingredients";
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
                // Ambiguous body before headers: accumulate as ingredients.
                ingredients.push(normalize_line(t.trim_start_matches(['-', '*', '•'])));
            }
        }
    }

    // If we never saw Ingredients: but body collected lines and we have no steps header,
    // keep body-as-ingredients. If we have steps only, ok.
    let mut recipe = Recipe::new(sanitize(&title));
    recipe.ingredients = ingredients;
    recipe.steps = steps;
    recipe
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
        assert_eq!(recipes.len(), 2, "got: {:?}", recipes.iter().map(|r| &r.title).collect::<Vec<_>>());
        let titles: HashSet<_> = recipes.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.contains("Fluffy Pancakes"));
        assert!(titles.contains("Simple Omelette"));
        for r in &recipes {
            assert!(matches!(r.source, RecipeSource::Epub { .. }));
            assert!(is_cookable(r));
        }
    }

    fn write_sample_epub(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
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

        zip.start_file("OEBPS/Text/recipes.xhtml", deflated).unwrap();
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
