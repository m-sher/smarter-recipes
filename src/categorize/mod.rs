//! LLM-assisted recipe category backfill (Gemini Flash via REST).
//!
//! Labels missing `meta.category` values so the plan-time category filter can
//! keep meals and drop components/drinks. Dry-run by default; `--apply` writes.
//!
//! When the model is unsure it returns an empty `categories` list — we leave
//! `meta.category` unset. With any category filter configured, uncategorized
//! recipes are excluded from the plan pool (same as blacklisted components).

mod gemini;
mod prompt;
mod vocab;

pub use gemini::{resolve_api_key, GeminiLabeler};
pub use vocab::{filter_labels, join_labels, ALLOWED_LABELS};

use crate::domain::{Recipe, RecipeSource};
use crate::storage::Store;
use anyhow::{bail, Result};
use std::io::{self, IsTerminal, Write};

/// Default Gemini model: cheapest current stable Flash-Lite. Override with `--model`.
pub const DEFAULT_MODEL: &str = "gemini-2.5-flash-lite";
/// Default recipes per API request.
pub const DEFAULT_BATCH_SIZE: usize = 25;
/// Default dry-run live sample size.
pub const DEFAULT_SAMPLE: usize = 8;
/// Prompt for confirmation when applying more than this many labels.
const CONFIRM_THRESHOLD: usize = 100;

/// One recipe's payload for the model (kept small).
#[derive(Debug, Clone)]
pub struct RecipeSnippet {
    pub id: String,
    pub title: String,
    pub ingredients: Vec<String>,
    pub source_kind: &'static str,
}

/// Model output for one recipe (categories already allowlisted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelResult {
    pub id: String,
    pub categories: Vec<String>,
}

/// Provider of batch labels (Gemini or test double).
pub trait CategoryLabeler {
    fn label_batch(&self, items: &[RecipeSnippet]) -> Result<Vec<LabelResult>>;
}

/// Source filter for eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFilter {
    #[default]
    All,
    Epub,
    Url,
}

impl SourceFilter {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "all" => Ok(Self::All),
            "epub" => Ok(Self::Epub),
            "url" => Ok(Self::Url),
            other => bail!("unknown --source '{other}' (use all, epub, or url)"),
        }
    }

    fn matches(self, source: &RecipeSource) -> bool {
        match self {
            Self::All => true,
            Self::Epub => matches!(source, RecipeSource::Epub { .. }),
            Self::Url => matches!(source, RecipeSource::Url { .. }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Epub => "epub",
            Self::Url => "url",
        }
    }
}

fn source_kind(source: &RecipeSource) -> &'static str {
    match source {
        RecipeSource::Epub { .. } => "epub",
        RecipeSource::Url { .. } => "url",
        RecipeSource::File { .. } => "file",
        RecipeSource::Image { .. } => "image",
        RecipeSource::Manual => "manual",
        RecipeSource::Unknown => "unknown",
    }
}

/// Options for a categorize run.
#[derive(Debug, Clone)]
pub struct CategorizeOptions {
    pub apply: bool,
    pub force: bool,
    pub limit: Option<usize>,
    pub batch_size: usize,
    pub sample: usize,
    pub source: SourceFilter,
    pub yes: bool,
    /// Optional single-recipe id prefix (unique match).
    pub id_prefix: Option<String>,
}

impl Default for CategorizeOptions {
    fn default() -> Self {
        Self {
            apply: false,
            force: false,
            limit: None,
            batch_size: DEFAULT_BATCH_SIZE,
            sample: DEFAULT_SAMPLE,
            source: SourceFilter::All,
            yes: false,
            id_prefix: None,
        }
    }
}

/// Summary of a categorize run.
#[derive(Debug, Default, Clone)]
pub struct CategorizeReport {
    pub eligible: usize,
    pub skipped_labeled: usize,
    pub labeled: usize,
    pub left_empty: usize,
    pub failed_batches: usize,
    pub written: usize,
    pub dry_run: bool,
}

/// Build a model snippet from a recipe (first 8 ingredient lines).
pub fn snippet_from_recipe(r: &Recipe) -> RecipeSnippet {
    RecipeSnippet {
        id: r.id.as_str().to_string(),
        title: r.title.clone(),
        ingredients: r
            .ingredients
            .iter()
            .take(8)
            .map(|l| l.original.clone())
            .collect(),
        source_kind: source_kind(&r.source),
    }
}

fn has_category(r: &Recipe) -> bool {
    r.meta
        .category
        .as_deref()
        .is_some_and(|c| c.split(',').any(|t| !t.trim().is_empty()))
}

/// Select eligible recipes: source filter, missing category (unless force), sort by id, limit.
pub fn select_eligible(recipes: &[Recipe], opts: &CategorizeOptions) -> (Vec<Recipe>, usize) {
    let mut skipped_labeled = 0usize;
    let mut eligible: Vec<Recipe> = recipes
        .iter()
        .filter(|r| opts.source.matches(&r.source))
        .filter(|r| {
            if opts.force || !has_category(r) {
                true
            } else {
                skipped_labeled += 1;
                false
            }
        })
        .cloned()
        .collect();

    if let Some(pfx) = &opts.id_prefix {
        eligible.retain(|r| r.id.as_str().starts_with(pfx.as_str()));
    }

    eligible.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

    if let Some(limit) = opts.limit {
        eligible.truncate(limit);
    }

    (eligible, skipped_labeled)
}

fn confirm_apply(n: usize, yes: bool) -> Result<bool> {
    if n <= CONFIRM_THRESHOLD || yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        bail!(
            "refusing to label {n} recipes without a TTY; pass --yes to confirm \
             (or use --limit for a smaller batch)"
        );
    }
    eprint!("Write categories for {n} recipes? [y/N] ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let ok = matches!(line.trim().to_lowercase().as_str(), "y" | "yes");
    if !ok {
        eprintln!("Aborted.");
    }
    Ok(ok)
}

/// Run categorize against `store` using `labeler`.
///
/// `eligible` should come from a single [`select_eligible`] call (caller owns
/// the preview counts). Progress and diagnostics go to stderr; caller prints
/// the final summary.
pub fn run_categorize(
    store: &Store,
    labeler: &dyn CategoryLabeler,
    opts: &CategorizeOptions,
    mut eligible: Vec<Recipe>,
    skipped_labeled: usize,
) -> Result<CategorizeReport> {
    let mut report = CategorizeReport {
        eligible: eligible.len(),
        skipped_labeled,
        dry_run: !opts.apply,
        ..Default::default()
    };

    if eligible.is_empty() {
        return Ok(report);
    }

    let batch_size = opts.batch_size.max(1);

    if !opts.apply {
        // Dry-run: optional live sample.
        let sample_n = opts.sample.min(eligible.len());
        if sample_n > 0 {
            let sample: Vec<RecipeSnippet> = eligible[..sample_n]
                .iter()
                .map(snippet_from_recipe)
                .collect();
            match labeler.label_batch(&sample) {
                Ok(results) => {
                    eprintln!("Sample labels ({sample_n} recipes, not saved):");
                    for (r, res) in eligible[..sample_n].iter().zip(results.iter()) {
                        let label = if res.categories.is_empty() {
                            "(empty — leave uncategorized)".to_string()
                        } else {
                            res.categories.join(", ")
                        };
                        eprintln!(
                            "  {}  {:<40}  → {label}",
                            short_id(r.id.as_str()),
                            truncate_chars(&r.title, 40),
                        );
                    }
                }
                Err(e) => {
                    eprintln!("Sample labeling failed (counts above still valid): {e:#}");
                    report.failed_batches += 1;
                }
            }
        }
        return Ok(report);
    }

    // Apply path
    if !confirm_apply(eligible.len(), opts.yes)? {
        return Ok(report);
    }

    let total = eligible.len();
    let mut labeled = 0usize;
    let mut left_empty = 0usize;
    let mut failed_batches = 0usize;
    let mut to_save: Vec<Recipe> = Vec::new();

    for (batch_i, chunk) in eligible.chunks_mut(batch_size).enumerate() {
        let snippets: Vec<RecipeSnippet> = chunk.iter().map(snippet_from_recipe).collect();
        match labeler.label_batch(&snippets) {
            Ok(results) => {
                let by_id: std::collections::HashMap<&str, &LabelResult> =
                    results.iter().map(|r| (r.id.as_str(), r)).collect();
                for r in chunk.iter_mut() {
                    let cats = by_id
                        .get(r.id.as_str())
                        .map(|x| x.categories.clone())
                        .unwrap_or_default();
                    if let Some(joined) = join_labels(&cats) {
                        r.meta.category = Some(joined);
                        labeled += 1;
                        to_save.push(r.clone());
                    } else {
                        left_empty += 1;
                    }
                }
            }
            Err(e) => {
                failed_batches += 1;
                eprintln!(
                    "  batch {} failed ({} recipes): {e:#}",
                    batch_i + 1,
                    chunk.len()
                );
            }
        }
        let done = ((batch_i + 1) * batch_size).min(total);
        eprint!("\r  [{done}/{total}] labeled…");
        let _ = io::stderr().flush();
    }
    eprintln!();

    if !to_save.is_empty() {
        let save_total = to_save.len();
        store.save_recipes(&to_save, |done| {
            eprintln!("  saved {done}/{save_total}");
        })?;
        report.written = save_total;
    }

    report.labeled = labeled;
    report.left_empty = left_empty;
    report.failed_batches = failed_batches;
    Ok(report)
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Recipe;
    use crate::normalize::normalize_line;
    use crate::storage::Store;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct MapLabeler {
        map: HashMap<String, Vec<String>>,
        calls: Mutex<usize>,
    }

    impl CategoryLabeler for MapLabeler {
        fn label_batch(&self, items: &[RecipeSnippet]) -> Result<Vec<LabelResult>> {
            *self.calls.lock().unwrap() += 1;
            Ok(items
                .iter()
                .map(|i| LabelResult {
                    id: i.id.clone(),
                    categories: self.map.get(&i.id).cloned().unwrap_or_default(),
                })
                .collect())
        }
    }

    fn rec(title: &str, ings: &[&str]) -> Recipe {
        let mut r = Recipe::new(title);
        r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
        r
    }

    #[test]
    fn select_skips_labeled_unless_force() {
        let mut a = rec("A", &["1 egg"]);
        a.meta.category = Some("Dinner".into());
        let b = rec("B", &["1 egg"]);
        let recipes = vec![a, b];
        let (el, skipped) = select_eligible(
            &recipes,
            &CategorizeOptions {
                force: false,
                ..Default::default()
            },
        );
        assert_eq!(el.len(), 1);
        assert_eq!(el[0].title, "B");
        assert_eq!(skipped, 1);

        let (el2, skipped2) = select_eligible(
            &recipes,
            &CategorizeOptions {
                force: true,
                ..Default::default()
            },
        );
        assert_eq!(el2.len(), 2);
        assert_eq!(skipped2, 0);
    }

    #[test]
    fn select_respects_limit_and_sorts_by_id() {
        let mut recipes = vec![];
        for i in [3, 1, 2] {
            let mut r = rec(&format!("R{i}"), &["1 egg"]);
            r.id = crate::domain::RecipeId::from(format!("id-{i}"));
            recipes.push(r);
        }
        let (el, _) = select_eligible(
            &recipes,
            &CategorizeOptions {
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(el.len(), 2);
        assert_eq!(el[0].id.as_str(), "id-1");
        assert_eq!(el[1].id.as_str(), "id-2");
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let mut r = rec("Cooler", &["1 cup lemonade"]);
        r.id = crate::domain::RecipeId::from("cool-1");
        store.save_recipe(&r).unwrap();

        let labeler = MapLabeler {
            map: HashMap::from([("cool-1".into(), vec!["Beverage".into()])]),
            calls: Mutex::new(0),
        };
        let opts = CategorizeOptions {
            apply: false,
            sample: 1,
            ..Default::default()
        };
        let all = store.list_recipes(None).unwrap();
        let (eligible, skipped) = select_eligible(&all, &opts);
        let report = run_categorize(&store, &labeler, &opts, eligible, skipped).unwrap();
        assert!(report.dry_run);
        assert_eq!(report.written, 0);
        assert_eq!(*labeler.calls.lock().unwrap(), 1);
        let loaded = store.get_recipe("cool-1").unwrap().unwrap();
        assert!(loaded.meta.category.is_none());
    }

    #[test]
    fn apply_writes_category() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let mut r = rec("Cooler", &["1 cup lemonade"]);
        r.id = crate::domain::RecipeId::from("cool-1");
        store.save_recipe(&r).unwrap();

        let labeler = MapLabeler {
            map: HashMap::from([("cool-1".into(), vec!["Beverage".into()])]),
            calls: Mutex::new(0),
        };
        let opts = CategorizeOptions {
            apply: true,
            sample: 0,
            yes: true,
            ..Default::default()
        };
        let all = store.list_recipes(None).unwrap();
        let (eligible, skipped) = select_eligible(&all, &opts);
        let report = run_categorize(&store, &labeler, &opts, eligible, skipped).unwrap();
        assert_eq!(report.written, 1);
        assert_eq!(report.labeled, 1);
        let loaded = store.get_recipe("cool-1").unwrap().unwrap();
        assert_eq!(loaded.meta.category.as_deref(), Some("Beverage"));
    }

    #[test]
    fn apply_empty_label_leaves_uncategorized() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let mut r = rec("Mystery", &["1 pinch dust"]);
        r.id = crate::domain::RecipeId::from("m-1");
        store.save_recipe(&r).unwrap();

        let labeler = MapLabeler {
            map: HashMap::new(),
            calls: Mutex::new(0),
        };
        let opts = CategorizeOptions {
            apply: true,
            yes: true,
            sample: 0,
            ..Default::default()
        };
        let all = store.list_recipes(None).unwrap();
        let (eligible, skipped) = select_eligible(&all, &opts);
        let report = run_categorize(&store, &labeler, &opts, eligible, skipped).unwrap();
        assert_eq!(report.left_empty, 1);
        assert_eq!(report.written, 0);
        assert!(store
            .get_recipe("m-1")
            .unwrap()
            .unwrap()
            .meta
            .category
            .is_none());
    }
}
