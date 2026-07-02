# Recipe Dedup (Scrape + Planner) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the same recipe from being stored or scheduled multiple times when scraped from different URLs (especially category/listing pages), using layered identity checks and TDD.

**Architecture:** Defense in depth. (1) Do not *import* listing/index URLs as recipes (still allow BFS link expansion from them). (2) Prefer schema.org canonical recipe URLs for provenance/identity. (3) On save, skip if normalized source URL **or** normalized title already exists. (4) Planner `normalize_pool` also collapses by title key so a dirty DB cannot reintroduce same-title meals. Do **not** add `category` to the crawl deny-list: that would block useful discovery edges; listing detection is an *import* policy, not a *link* policy.

**Tech Stack:** Rust, existing `src/ingest/crawl.rs` / `url.rs`, `src/storage/mod.rs`, `src/planning/mod.rs`, `src/cli/mod.rs`, `cargo test`.

## Global Constraints

- TDD: red → green → refactor for every behavioral change; run the named test(s) before implementing.
- Prefer pure functions with unit tests over SQLite for identity/listing helpers; use `Store` tests for persistence lookups.
- First-wins on collision (keep existing row; skip new scrape). No automatic merge of ingredient lists.
- Title identity key: lowercase, trim, collapse internal whitespace, normalize common apostrophe variants (`'` `'` `'` → `'`).
- YAGNI: no fuzzy title matching, no ingredient-set fingerprint, no CLI `dedupe` command in this plan (one-shot cleanup already applied to the local DB).
- Existing user DB was cleaned of title-dupes (209 recipes, 0 title groups with count > 1) on 2026-07-02; re-scrape without these fixes would reintroduce garbage.

## Background (why the plan output looked wrong)

- Planner only dedupes by `RecipeId` (`planning/mod.rs` `normalize_pool`).
- Every ingest mints a new UUID (`RecipeId::new()`).
- Scrape skip set is **fetch URL only** (`cli/mod.rs` builds skip from `recipe_source_url` + failures).
- Category pages (`/category/...`, `/page/N`) often embed JSON-LD for a featured recipe; crawl treated them as successful imports with **different** source URLs → identical titles, different ids → “Grilled S'mores” four times in one plan.

## File map

| File | Role |
|------|------|
| `src/ingest/crawl.rs` | `is_listing_url`; scrape path: listing fetch → expand links, do not push to `recipes` |
| `src/ingest/url.rs` | Extract JSON-LD `url` / `@id` into `meta.source_url`; prefer as identity when present |
| `src/ingest/mod.rs` | Re-export `is_listing_url`, `normalize_title_key` if public |
| `src/domain/recipe.rs` or `src/ingest/crawl.rs` | `normalize_title_key` (pure; domain is fine if used by storage+planning) |
| `src/storage/mod.rs` | `find_id_by_source_url`, `find_id_by_title_key` |
| `src/cli/mod.rs` | After scrape, skip save when URL or title already present; count skips |
| `src/planning/mod.rs` | Title-key collapse in `normalize_pool` |

---

### Task 1: Listing URL classifier (pure)

**Files:**
- Modify: `src/ingest/crawl.rs` (near `is_denied_url` / `DENY_SEGMENTS`)
- Test: same file `#[cfg(test)]` module (existing pattern)
- Modify: `src/ingest/mod.rs` — `pub use crawl::is_listing_url` (optional if tests only need crate-internal)

**Interfaces:**
- Produces: `pub fn is_listing_url(u: &str) -> bool`

**Behavior:**
- `true` when path (case-insensitive) has a segment `category` or `categories`, or `tag`/`tags`, or matches pagination `page` + numeric segment (e.g. `/page/2`, `/.../page/2`), or path is empty/`/` only, or final segment is a bare archive-style `author`/`authors` listing (already partly denied for *discovery* — still classify for import).
- `false` for normal recipe slugs like `https://itsahero.com/chicken-tortellini-skillet`.
- Invalid URLs → `true` (do not import garbage) **or** `false` consistently with tests — pick **`true`** (fail closed for import).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn listing_urls_detected() {
    assert!(is_listing_url("https://itsahero.com/category/food"));
    assert!(is_listing_url("https://itsahero.com/category/crafty/recipes/dairy-free/page/2"));
    assert!(is_listing_url("https://site.com/page/2"));
    assert!(is_listing_url("https://site.com/tag/summer"));
    assert!(is_listing_url("https://site.com/"));
}

#[test]
fn recipe_urls_not_listings() {
    assert!(!is_listing_url("https://itsahero.com/chicken-tortellini-skillet"));
    assert!(!is_listing_url(
        "https://itsahero.com/delicious-air-fryer-salsa-verde-recipe"
    ));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -q is_listing_url -- --nocapture 2>&1 | tail -20`  
(or `cargo test listing_urls_detected recipe_urls_not_listings`)  
Expected: compile error `is_listing_url` not found, or FAIL.

- [ ] **Step 3: Minimal implementation**

```rust
/// True when the URL is a category/tag/pagination/index page that must not
/// be stored as a recipe (JSON-LD on those pages is usually a featured recipe).
/// Still allowed as a BFS node for link discovery.
pub fn is_listing_url(u: &str) -> bool {
    let Ok(url) = Url::parse(u) else {
        return true;
    };
    let path = url.path().to_lowercase();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return true;
    }
    const LISTING_SEGMENTS: &[&str] = &[
        "category", "categories", "tag", "tags", "author", "authors",
    ];
    if segments.iter().any(|s| LISTING_SEGMENTS.contains(s)) {
        return true;
    }
    // /page/2 or .../page/2
    segments.windows(2).any(|w| w[0] == "page" && w[1].chars().all(|c| c.is_ascii_digit()))
        || (segments.last().is_some_and(|s| *s == "page"))
}
```

Refine edge cases only if a test fails (e.g. a real recipe slug containing the word `category` — unlikely; document if we special-case).

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test listing_urls_detected recipe_urls_not_listings -- --nocapture`

- [ ] **Step 5: Commit**

```bash
git add src/ingest/crawl.rs src/ingest/mod.rs
git commit -m "test+feat: classify listing URLs for scrape import policy"
```

---

### Task 2: Scrape must not import listing pages (still expand links)

**Files:**
- Modify: `src/ingest/crawl.rs` — inside `scrape_new_recipes` batch result handling (~line 333)
- Test: `src/ingest/crawl.rs` tests with `MapFetcher`

**Interfaces:**
- Consumes: `is_listing_url`, existing `ingest_html` / link expansion
- Produces: listing URLs appear in `not_recipe` (or a clear reason string), **not** in `outcome.recipes`; child links still enqueued when `depth` allows

**Behavior change in the fetch loop:** after a successful fetch, if `is_listing_url(&url)`, treat as not-a-recipe for import (`not_recipe.push((url, "listing/index page".into()))`) **without** calling recipe extract as a success path. Prefer:

```rust
let ingest = if is_listing_url(&url) {
    Err("listing/index page".to_string())
} else {
    match ingest_html(/* existing */) { ... }
};
```

Keep link expansion on the HTML regardless (current code expands when `fetch_ok && !html.is_empty()`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn listing_page_with_json_ld_is_not_imported_but_links_expand() {
    let mut pages = HashMap::new();
    // Index links only to a category page.
    pages.insert(
        "https://site.com/recipes".to_string(),
        r#"<a href="/category/food">Food</a>"#.to_string(),
    );
    // Category page has full Recipe JSON-LD AND a link to a real recipe.
    pages.insert(
        "https://site.com/category/food".to_string(),
        format!(
            r#"<html><body>
              <script type="application/ld+json">{{
                "@type":"Recipe","name":"Grilled S'mores",
                "recipeIngredient":["bread","chocolate"]
              }}</script>
              <a href="/grilled-smores">real</a>
            </body></html>"#
        ),
    );
    pages.insert(
        "https://site.com/grilled-smores".to_string(),
        recipe_html("Grilled S'mores"),
    );
    let f = MapFetcher { pages };
    let out = scrape(&f, "https://site.com/recipes", 10, &HashSet::new(), 2);
    assert!(
        out.recipes.iter().all(|r| {
            recipe_source_url(r).map(|u| !u.contains("/category/")).unwrap_or(true)
        }),
        "must not import category URL as recipe source: {:?}",
        out.recipes.iter().map(|r| r.title.clone()).collect::<Vec<_>>()
    );
    // Real recipe page should still be reachable via BFS.
    assert!(
        out.recipes.iter().any(|r| r.title == "Grilled S'mores"),
        "expected real recipe via expanded link, got {:?}",
        out.recipes.iter().map(|r| &r.title).collect::<Vec<_>>()
    );
    assert!(
        out.not_recipe.iter().any(|(u, _)| u.contains("/category/food")),
        "category page should be classified not_recipe"
    );
}
```

Adjust `recipe_html` helper if it already emits JSON-LD (reuse existing test helper in this module).

- [ ] **Step 2: Run test — expect FAIL** (category imported as recipe, or real recipe missing)

Run: `cargo test listing_page_with_json_ld_is_not_imported -- --nocapture`

- [ ] **Step 3: Implement scrape branch for listings**

In the worker or post-fetch match arm: short-circuit import when `is_listing_url(&url)`.

- [ ] **Step 4: Run test — expect PASS**

Also run full crawl suite: `cargo test ingest::crawl -- --nocapture`

- [ ] **Step 5: Commit**

```bash
git add src/ingest/crawl.rs
git commit -m "fix: do not import listing pages as recipes during scrape"
```

---

### Task 3: Canonical URL from JSON-LD

**Files:**
- Modify: `src/ingest/url.rs` — `recipe_from_json_ld`
- Test: `src/ingest/url.rs` or new unit tests using `UrlSource { offline_html: Some(...), .. }`

**Interfaces:**
- Produces: when JSON-LD has string `url` or absolute `@id` that looks like `http(s)`, set `recipe.meta.source_url` to that value; `RecipeSource::Url` may still record the **fetch** URL (provenance of the fetch) **or** use canonical for both — **decision for this plan:**  
  - `source = RecipeSource::Url { url: fetch_url }` (what we hit)  
  - `meta.source_url = canonical.unwrap_or(fetch_url)`  
  - `recipe_source_url` already prefers `RecipeSource::Url` over meta — **change identity preference:** for skip/dedupe, prefer `meta.source_url` when set, else source URL.  
  - **Simpler alternative (preferred for fewer moving parts):** set **both** `RecipeSource::Url.url` and `meta.source_url` to the **canonical** when present, so existing `recipe_source_url` + skip set work without CLI changes beyond title checks. Document that re-fetch of a listing no longer stores listing URL.

**Preferred simpler rule:** If JSON-LD provides a usable canonical URL, store that as `RecipeSource::Url { url }` and `meta.source_url`; otherwise keep fetch URL.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn json_ld_canonical_url_overrides_fetch_url() {
    let html = r#"
    <script type="application/ld+json">
    {
      "@type": "Recipe",
      "name": "Test Cake",
      "url": "https://example.com/test-cake",
      "recipeIngredient": ["1 cup flour"]
    }
    </script>"#;
    let src = UrlSource {
        offline_html: Some(html.into()),
        ..Default::default()
    };
    let recipe = src.ingest("https://example.com/category/dessert").unwrap();
    assert_eq!(
        recipe_source_url(&recipe).as_deref(),
        Some("https://example.com/test-cake")
    );
}
```

(Import `recipe_source_url` from crawl or test via `meta` / `source` fields directly.)

- [ ] **Step 2: Run — expect FAIL** (source still category URL)

- [ ] **Step 3: In `recipe_from_json_ld`, set meta.source_url from `url` / `@id`; in `UrlSource::ingest`, if `meta.source_url` is set, use it for `RecipeSource::Url`**

```rust
// recipe_from_json_ld, after building meta:
if let Some(u) = obj
    .get("url")
    .and_then(|v| v.as_str())
    .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
{
    meta.source_url = Some(u.to_string());
} else if let Some(u) = obj
    .get("@id")
    .and_then(|v| v.as_str())
    .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
{
    meta.source_url = Some(u.to_string());
}
```

```rust
// UrlSource::ingest after extract:
let identity = recipe
    .meta
    .source_url
    .clone()
    .unwrap_or_else(|| url.to_string());
recipe.source = RecipeSource::Url {
    url: identity.clone(),
};
recipe.meta.source_url = Some(identity);
```

- [ ] **Step 4: Tests pass; run any existing url ingest tests**

Run: `cargo test json_ld_canonical -- --nocapture` and `cargo test url::`

- [ ] **Step 5: Commit**

```bash
git add src/ingest/url.rs
git commit -m "feat: prefer JSON-LD canonical URL as recipe source identity"
```

---

### Task 4: `normalize_title_key` pure helper

**Files:**
- Create helper in `src/domain/recipe.rs` (shared by storage + planning) **or** `src/domain/mod.rs` — prefer `recipe.rs` next to `Recipe`
- Export via `domain/mod.rs` if needed

**Interfaces:**
- Produces: `pub fn normalize_title_key(title: &str) -> String`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn title_key_normalizes_case_space_apostrophe() {
    assert_eq!(
        normalize_title_key("  Grilled S'mores  "),
        normalize_title_key("grilled s'mores")
    );
    assert_eq!(
        normalize_title_key("Grilled S\u{2019}mores"), // right single quotation mark
        normalize_title_key("Grilled S'mores")
    );
    assert_eq!(normalize_title_key("A   B"), "a b");
}
```

- [ ] **Step 2: Run — FAIL**

- [ ] **Step 3: Implement**

```rust
pub fn normalize_title_key(title: &str) -> String {
    let mut s = title.trim().to_lowercase();
    for ch in ['\u{2018}', '\u{2019}', '\u{02BC}'] {
        s = s.replace(ch, "'");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
```

- [ ] **Step 4: PASS + commit**

```bash
git add src/domain/recipe.rs src/domain/mod.rs
git commit -m "feat: normalize_title_key for recipe identity"
```

---

### Task 5: Store lookups by source URL and title key

**Files:**
- Modify: `src/storage/mod.rs`
- Test: existing `#[cfg(test)]` in that module

**Interfaces:**
- Produces:
  - `pub fn find_id_by_normalized_source_url(&self, url: &str) -> Result<Option<String>>`
  - `pub fn find_id_by_title_key(&self, title_key: &str) -> Result<Option<String>>`

**Implementation notes:**
- Source URL is inside `source_json` / `meta_json` as JSON text today. Options: (a) scan `list_recipes` in Rust and compare `normalize_url(recipe_source_url(...))` — fine for hundreds of recipes; (b) SQL `json_extract`. Prefer **in-Rust scan via `list_recipes(None)`** for simplicity and reuse of `normalize_url` / `recipe_source_url`, unless too slow — YAGNI for household DB.
- Title: compare `normalize_title_key(&r.title) == title_key`.
- Return first matching id (stable order = list order).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn find_by_source_url_and_title_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("t.db")).unwrap();
    let mut r = Recipe::new("Grilled S'mores");
    r.source = RecipeSource::Url {
        url: "https://example.com/grilled-smores".into(),
    };
    r.meta.source_url = Some("https://example.com/grilled-smores".into());
    store.save_recipe(&r).unwrap();

    let by_url = store
        .find_id_by_normalized_source_url("https://example.com/grilled-smores/")
        .unwrap();
    assert_eq!(by_url.as_deref(), Some(r.id.as_str()));

    let by_title = store
        .find_id_by_title_key(&normalize_title_key("GRILLED S'MORES"))
        .unwrap();
    assert_eq!(by_title.as_deref(), Some(r.id.as_str()));

    assert!(store
        .find_id_by_title_key(&normalize_title_key("Other"))
        .unwrap()
        .is_none());
}
```

(Use `tempfile` if already a dev-dep; else existing test pattern with `tempdir` in storage tests — match current tests in file.)

- [ ] **Step 2: FAIL → implement → PASS**

- [ ] **Step 3: Commit**

```bash
git add src/storage/mod.rs
git commit -m "feat: store lookup by source URL and title key"
```

---

### Task 6: CLI scrape / import skip on existing URL or title

**Files:**
- Modify: `src/cli/mod.rs` — `Commands::Scrape` save loop; optionally single `import` of URL for consistency

**Interfaces:**
- Consumes: `find_id_by_normalized_source_url`, `find_id_by_title_key`, `normalize_title_key`, `recipe_source_url`, `normalize_url`
- Produces: printed skip counts; no second row for same title/URL

**Logic for each scraped recipe before `save_recipe`:**

```rust
let mut skipped_dup = 0usize;
for recipe in &outcome.recipes {
    if let Some(u) = recipe_source_url(recipe) {
        if store
            .find_id_by_normalized_source_url(&normalize_url(&u))?
            .is_some()
        {
            skipped_dup += 1;
            continue;
        }
    }
    if store
        .find_id_by_title_key(&normalize_title_key(&recipe.title))?
        .is_some()
    {
        skipped_dup += 1;
        continue;
    }
    if !dry_run {
        store.save_recipe(recipe)?;
        // existing clear_scrape_failure ...
    }
}
// include skipped_dup in summary line
```

Also extend the pre-scrape `skip` set: still URL-based for fetch avoidance. Title cannot be known pre-fetch.

- [ ] **Step 1: Integration-style test preferred in storage+crawl unit tests rather than CLI** — if no CLI test harness, add a small **library function** in `ingest` or `storage`:

```rust
/// Returns true if this recipe should not be inserted (URL or title collision).
pub fn recipe_already_known(store: &Store, recipe: &Recipe) -> Result<bool> {
    if let Some(u) = recipe_source_url(recipe) {
        if store
            .find_id_by_normalized_source_url(&normalize_url(&u))?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(store
        .find_id_by_title_key(&normalize_title_key(&recipe.title))?
        .is_some())
}
```

Place on `Store` as `pub fn is_duplicate_recipe(&self, recipe: &Recipe) -> Result<bool>` to avoid circular deps (storage would need crawl's `recipe_source_url` — better put helper in `ingest/mod.rs` or `cli` only).

**Avoid storage → ingest cycle:** implement skip helper in `cli/mod.rs` as a private fn, tested indirectly via a **unit test module in cli** only if exists; otherwise test `Store` lookups thoroughly (Task 5) and keep CLI thin.

Alternative testable design: add `Store::is_duplicate(&self, title: &str, source_url: Option<&str>)` with only string args — no cycle.

```rust
pub fn is_duplicate(&self, title: &str, source_url: Option<&str>) -> Result<bool> {
    if let Some(u) = source_url {
        if self.find_id_by_normalized_source_url(&normalize_url(u))?.is_some() {
            return Ok(true);
        }
    }
    Ok(self.find_id_by_title_key(&normalize_title_key(title))?.is_some())
}
```

But `normalize_url` lives in ingest — either move `normalize_url` to a small `src/ingest/url_norm.rs` used by storage, **or** pass already-normalized URL into `find_id_*` only and normalize in CLI.

**Plan decision:** Keep `normalize_url` in crawl; CLI normalizes before calling `find_id_by_normalized_source_url`. Storage compares with `normalize_url` only if we re-export and allow storage to depend on ingest — **today storage does not depend on ingest**. So storage should either:
1. Store raw and compare with a **duplicated** minimal normalize, or
2. Compare by scanning and calling a callback, or
3. Put `normalize_url` + `normalize_title_key` in `domain` or new `src/identity.rs`.

**Cleanest:** move `normalize_url` to `src/domain/url_norm.rs` or keep title in domain and URL normalize in domain too (both pure). For this plan, **duplicate is worse** — extract:

- `src/domain/identity.rs` with `normalize_title_key` + re-export/move `normalize_url` from crawl into domain (or `src/util`).

**Adjusted file map for Task 4/5:** if Task 4 put title in domain, Task 3/5/6 may move `normalize_url` to `src/domain/url_norm.rs` and have crawl re-export for compatibility. Do that as part of Task 5 if storage needs it; otherwise CLI-only normalize and storage does:

```rust
// storage: compare normalize_url-equivalent by trimming/lowercasing path — call into domain
use crate::domain::normalize_url; // after move
```

- [ ] **Step 1:** If needed, **move** `normalize_url` to `domain` (or `identity` module) with existing crawl tests still compiling via re-export. TDD: move with tests green (refactor commit).

- [ ] **Step 2:** Add `Store::is_duplicate_of(&self, title: &str, source_url: Option<&str>) -> Result<bool>` tests.

- [ ] **Step 3:** Wire CLI scrape loop; update summary string to mention title/URL skips.

- [ ] **Step 4:** `cargo test` full suite.

- [ ] **Step 5: Commit**

```bash
git commit -m "feat: skip scrape saves on existing source URL or title"
```

---

### Task 7: Planner title-key dedupe (last line of defense)

**Files:**
- Modify: `src/planning/mod.rs` — `normalize_pool`
- Test: existing planning tests module

**Interfaces:**
- Consumes: `normalize_title_key`
- Behavior: first wins on `(recipe_id)` **and** on `title_key`; empty-ingredient filter unchanged

```rust
fn normalize_pool(pool: &[Recipe]) -> (Vec<&Recipe>, Vec<HashSet<IngredientKey>>) {
    let mut seen_ids = HashSet::new();
    let mut seen_titles = HashSet::new();
    // ...
    for r in pool {
        if !seen_ids.insert(r.id.as_str()) {
            continue;
        }
        let title_key = normalize_title_key(&r.title);
        if !title_key.is_empty() && !seen_titles.insert(title_key) {
            continue;
        }
        recipes.push(r);
        keys.push(recipe_keys(r));
    }
    // ... empty filter
}
```

- [ ] **Step 1: Failing test**

```rust
#[test]
fn duplicate_titles_different_ids_collapse() {
    let mut a = rec_with_id("id-a", "Grilled S'mores", &["1 bread"]);
    let b = rec_with_id("id-b", "grilled s'mores", &["1 bread", "1 chocolate"]);
    let plan = plan_meals(
        &[a, b],
        &PlanOptions {
            days: 2,
            meals_per_day: 1,
        },
    );
    assert_eq!(plan.meals.len(), 1);
    assert_eq!(plan.meals[0].recipe_id.as_str(), "id-a"); // first wins
}
```

- [ ] **Step 2: FAIL → implement → PASS**

- [ ] **Step 3: Update module docs** at top of `planning/mod.rs` to say “no repeats by recipe id **or** normalized title”.

- [ ] **Step 4: Commit**

```bash
git add src/planning/mod.rs
git commit -m "fix: planner collapses pool by normalized title as well as id"
```

---

### Task 8: Full verification + light docs

**Files:**
- Modify: `README.md` — one short note under scrape/plan that recipes are unique by source URL and title

- [ ] **Step 1: Run full test suite**

```bash
cargo test
```

Expected: all pass.

- [ ] **Step 2: Manual smoke (optional, needs network)**

```bash
# against a temp db
SMARTER_RECIPES_DB=/tmp/sr-dedup-test.db cargo run -- scrape --url https://itsahero.com/ --limit 30 --depth 2
SMARTER_RECIPES_DB=/tmp/sr-dedup-test.db cargo run -- plan --days 5 --per-day 2
# Inspect: no repeated titles; sqlite count distinct titles == count(*)
```

- [ ] **Step 3: README blurb + commit**

```bash
git add README.md
git commit -m "docs: note scrape/plan recipe identity (URL + title)"
```

---

## Out of scope (explicit)

- Deleting remaining **non-dupe** category-only rows still in the user DB (~21 after title-dupe cleanup) — optional follow-up one-shot or `dedupe --listings` command.
- Fuzzy matching / ingredient fingerprints.
- Changing min-union objective.
- Graphite/PR automation.

## Self-review

| Requirement | Task |
|-------------|------|
| Don’t re-import same recipe via different listing URLs | 1, 2, 3, 6 |
| Prefer real recipe URL identity | 3 |
| Title-level identity at persistence | 4, 5, 6 |
| Planner won’t schedule same title twice | 7 |
| TDD throughout | Steps 1–4 each task |
| Existing DB cleaned | Done pre-plan (2026-07-02); not a code task |

**Type consistency:** `normalize_title_key(&str) -> String`; store methods return `Result<Option<String>>` (full id); listing classifier `is_listing_url(&str) -> bool`.

**Placeholder scan:** none intentional.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-02-recipe-dedup.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — this session, executing-plans with checkpoints  

Which approach?
