# smarter-recipes

CLI tool that ingests recipes from multiple sources, stores them in a local SQLite database, plans meals to **minimize distinct ingredients** (no recipe repeats; fewer shopping-list line items), and builds **optimized shopping lists** that minimize cost then leftover waste.

## Features

| Area | What you get |
|------|----------------|
| **Ingestion** | JSON / TOML / plain text files, web pages (schema.org `Recipe` JSON-LD with HTML fallback), images via Tesseract OCR or `.txt` sidecars, **EPUB cookbooks** (linked recipe index or TOC fallback) |
| **Normalization** | Free-text ingredient lines → name, quantity, unit; units converted to canonical g / ml / ea for aggregation |
| **Storage** | Embedded SQLite; ingredients deduplicated by `(name, unit kind)`; pantry stock by same identity |
| **Pantry** | Track on-hand ingredients; mark shopping results as purchased; plan and shop net of stock |
| **Planning** | Min-union scheduler (exact ILP via HiGHS over the full pool under nutrition constraints), no recipe repeats; pantry stock with quantity-aware binary shortfall; optional per-scope min/max **and macro-split ratio** bounds from TOML (per-day min/max also via CLI flags) (documented in `src/planning/mod.rs`) |
| **Shopping** | Package multiset optimization: **cost first**, then **minimum leftover**; requirements reduced by pantry (documented in `src/shopping/mod.rs`) |
| **Extensibility** | New ingest sources implement `RecipeSourceIngest`; custom package catalogs via JSON overlay |

## Requirements

- **Rust** 1.74+ (edition 2021)
- **CMake, a C++ compiler, and libclang** — [HiGHS](https://highs.dev/) (the exact meal-plan solver) is built from source. Debian/Ubuntu: `sudo apt install cmake g++ libclang-dev`.
- **Optional:** [Tesseract OCR](https://github.com/tesseract-ocr/tesseract) for image import (`tesseract` on `PATH`)
- Network access only for `import url …` (core logic runs fully offline)

No system SQLite required — `rusqlite` is built with the `bundled` feature.

## Setup

```bash
cargo build --release
# binary: target/release/smarter-recipes
```

Database default path:

- Linux/macOS: `$XDG_DATA_HOME/smarter-recipes/recipes.db` or `~/.local/share/smarter-recipes/recipes.db`
- Override with `--db PATH` or env `SMARTER_RECIPES_DB`


## Desktop GUI

A Tauri 2 desktop shell lives in [`desktop/`](desktop/). Same local SQLite DB as
the CLI. Pages: **Home**, **Library** (detail + delete), **Pantry** (add/remove),
**Plan** (nutrition options, ★ pantry, shop, restock), **Import** (file/url/epub).

```bash
cd desktop
npm install
# Linux: install WebKitGTK/GTK deps first (see desktop/README.md)
npm run tauri dev
```

### Visual regression tests

UI is tested **without** launching Tauri. Playwright serves the Vite app with
deterministic mock data and compares full-page screenshots to golden frames:

```bash
cd desktop
npx playwright install chromium   # once
npm run test:visual               # compare
npm run test:visual:update        # rewrite baselines after intentional UI changes
```

Goldens: `desktop/tests/visual/shell.spec.ts-snapshots/` (home, library, recipe,
pantry, plan-shop, import).

## Usage

```bash
# Import sample recipes
smarter-recipes import file recipes/pancakes.json
smarter-recipes import file recipes/french_toast.json
smarter-recipes import file recipes/tomato_pasta.json
smarter-recipes import file recipes/garlic_bread.toml
smarter-recipes import file recipes/chicken_rice.json
smarter-recipes import file recipes/omelette.txt

# From the web (JSON-LD preferred)
smarter-recipes import url 'https://example.com/recipe'

# From an image (needs tesseract, or recipe.png + recipe.txt sidecar)
smarter-recipes import image scans/recipe.png

# Auto-detect source from the input
smarter-recipes import auto recipes/pancakes.json

# Enter a recipe interactively (title, servings, ingredients, steps)
smarter-recipes import manual

# From an EPUB cookbook: uses a linked recipe index (or TOC fallback) to
# segment recipes — no Calibre. Page-number-only indexes are not supported.
# May import many recipes in one command.
smarter-recipes import epub path/to/cookbook.epub
smarter-recipes import auto path/to/cookbook.epub
smarter-recipes import epub path/to/cookbook.epub --dry-run

# Crawl a seed URL for same-host recipe pages (BFS). Works from a category page:
# links may point at site-root posts (not only path descendants of the seed).
# --depth N follows links from fetched pages up to N hops; --limit caps fetches;
# --jobs is concurrency. Category/nav pages are not remembered as failures, so
# re-runs can still walk them to find newly published recipes. Only hard fetch
# errors are persisted (skipped unless --retry-failed). Asset/author/tag URLs
# are deny-listed to save budget.
# Identity: recipes are unique by source URL and by title (normalized) — scrape
# skips already-known URLs/titles; the planner never schedules the same title twice.
smarter-recipes scrape 'https://example.com/recipes' --limit 10 --jobs 8
smarter-recipes scrape 'https://example.com/category/chicken' --depth 3 --limit 50
smarter-recipes scrape 'https://example.com/recipes' --dry-run
smarter-recipes scrape 'https://example.com/recipes' --retry-failed

# Search DuckDuckGo, then multi-host BFS from result URLs (same identity rules).
# --pages is how many SERP pages to load; --limit is site-page fetch budget;
# --depth is same-host expansion from each result (default 2).
#
# Tip: prefer specific dish/ingredient queries ("chicken parmesan recipe",
# "overnight oats"). Broad "best / ideas / high protein dinners" queries often
# land on JS-rendered listicle pages; static HTML crawling finds mostly site nav
# and may import few or no recipes. No headless browser — by design.
smarter-recipes search-scrape 'chicken parmesan recipe' --limit 50 --jobs 8 --depth 2 --pages 2
smarter-recipes search-scrape 'overnight oats' --limit 80 --dry-run

# Browse
smarter-recipes list
smarter-recipes list --filter pasta
smarter-recipes show <id-or-prefix>
smarter-recipes status

# Pantry: track what you already have (canonical g / ml / ea)
smarter-recipes pantry add '2 cups milk'
smarter-recipes pantry add '12 eggs'
smarter-recipes pantry set '500g flour'     # absolute quantity
smarter-recipes pantry list
smarter-recipes pantry remove milk          # optional: --kind volume
# smarter-recipes pantry clear --yes

# Plan 5 days, 1 meal/day, minimize distinct ingredients (no recipe-id or title repeats).
# On-hand pantry keys are treated as already covered when scoring plans.
smarter-recipes plan --days 5 --per-day 1

# Restrict the candidate pool
smarter-recipes plan --days 3 --pool <id1>,<id2>,<id3>

# Steer selection with estimated macro ranges (whole-recipe estimates).
# CLI flags set per-day bounds; a TOML file can also set per_meal and plan totals.
# Feasible plans are preferred; if none exist, the least-violation plan is kept
# with warnings. Estimates are trusted as-is; coverage is always printed.
smarter-recipes plan --days 5 --per-day 2 --min-protein-g 50 --max-kcal 3000
smarter-recipes plan --days 5 --nutrition-config examples/nutrition_bounds.toml
smarter-recipes plan --days 5 --nutrition-config examples/nutrition_bounds.toml --min-protein-g 60

# Steer slots by time of day using recipe tags and schema.org categories
# (breakfast / lunch / dinner; brunch→breakfast+lunch, supper→dinner).
# 1 meal/day: any; 2: breakfast then dinner; 3: B/L/D; 4+: first breakfast,
# last dinner, lunch near the middle, other slots unrestricted. Soft mismatches
# when the pool lacks labels; combines with nutrition bounds (nutrition ranks first).
smarter-recipes plan --days 3 --per-day 3 --tod
smarter-recipes plan --days 5 --per-day 2 --tod --min-protein-g 50

# Shopping list with package recommendations + leftover flags
# (amounts already in the pantry are subtracted / omitted)
smarter-recipes shop <plan-id-or-prefix>

# After buying *and cooking* a plan: add purchased packages, then deduct what
# the recipes consumed. Net pantry change is packaging leftover only.
# Each plan can be restocked once (idempotent guard).
smarter-recipes pantry restock <plan-id-or-prefix>

# Show how plan ordering introduces ingredients (trip analysis)
smarter-recipes shop <plan-id> --trips

# Enrich catalog from Open Food Facts (network) or a recorded fixture
smarter-recipes shop <plan-id> --fetch-prices openfoodfacts
smarter-recipes shop <plan-id> --fetch-prices fixture --store-fixture fixtures/store_catalog.json

# Custom package sizes/prices (JSON object: name → [packages])
smarter-recipes shop <plan-id> --catalog my_catalog.json

# Export / delete
smarter-recipes export <id> -o out.json
smarter-recipes delete <id>

# Re-parse stored recipes with the current parser (e.g. after upgrading).
# Reads each recipe's original ingredient text — no re-import needed.
smarter-recipes reparse <id>
smarter-recipes reparse --all

# Category labels for planning filters (blacklist/whitelist in nutrition TOML).
# With any filter configured, recipes with no category are excluded (same as
# blacklisted). Unsure model output stays empty → stays excluded from the pool.
# Plan rationale splits "blacklisted: N" vs "no category: M" so gaps are visible.
# URL recipes: refresh first to pull schema.org recipeCategory when present.
# EPUB / gaps: categorize uses Gemini Flash-Lite (cheap) to fill missing categories.
# Dry-run by default; --apply writes. API key: SMARTER_RECIPES_GEMINI_API_KEY
# or GEMINI_API_KEY (https://aistudio.google.com/apikey).
smarter-recipes refresh --all --apply
smarter-recipes categorize                    # dry-run + sample labels
smarter-recipes categorize --sample 0         # counts only (no network)
smarter-recipes categorize --limit 50 --apply # small first write
smarter-recipes categorize --apply --yes      # full backfill

# Nutrition: `plan` prints estimated per-day macros with explicit coverage
# ("N/M ingredients"); `show` prints per-serving nutrition when the source
# site published it. Estimates use a built-in USDA-style per-100 g table;
# resolve uncovered ingredient names into a local cache via USDA FoodData
# Central (set SMARTER_RECIPES_FDC_KEY for a personal key; DEMO_KEY default),
# falling back to keyless Open Food Facts when FDC is rate-limited, or an
# offline JSON fixture. Network misses are negative-cached and not retried;
# a fixture run re-checks every name and never writes the cache.
smarter-recipes nutrition fetch
smarter-recipes nutrition fetch --fixture my_profiles.json --limit 50
smarter-recipes nutrition clear-cache   # drop cached lookups to force a re-fetch
```

### Package catalog JSON overlay

```json
{
  "milk": [
    {
      "label": "1L milk",
      "size_canonical": 1000,
      "price_cents": 150,
      "kind": "volume"
    }
  ]
}
```

`size_canonical` is in **grams**, **milliliters**, or **each**, matching `kind`: `mass` | `volume` | `count` | `other`.

## Architecture

```
src/
  domain/       Shared types: Recipe, IngredientLine, MealPlan, PantryItem, units
  normalize/    Ingredient parsing + unit tables (no I/O)
  ingest/       Pluggable sources: file, url, ocr, epub (index segmentation), crawl
  storage/      SQLite persistence + ingredient dedup + pantry stock
  planning/     Min-union meal planner (no repeats; pantry-aware)
  shopping/     Package purchase optimizer (nets against pantry)
  pricing/      Package catalog, density table, and store sources (Open Food Facts / fixture)
  nutrition/    Per-100 g macro table, USDA FDC source + cache, plan/recipe estimates
  cli/          clap commands
```

**Design choices**

1. **Canonical units** — Mass→g, volume→ml, count→ea. Only same `UnitKind` quantities are summed.
2. **Ingredient identity** — `(normalized_name, UnitKind)` so “2 cups milk” and “500 ml milk” aggregate when both are volume. Pantry rows use the same key.
3. **Core vs I/O** — Normalization, planning, and purchase optimization are pure and unit-tested without network or OCR.
4. **New ingest source** — Implement `RecipeSourceIngest` in `ingest/`, wire it in `ingest_from`.
5. **Density table** — Volume-measured dry goods (flour, sugar, salt, …) convert to mass for realistic packages (`src/pricing/density.rs`).
6. **Store sources** — `ProductSource` trait with Open Food Facts + fixture backends (`--fetch-prices`). Graceful fallback to the offline catalog.
7. **Pantry** — On-hand stock feeds planning and shopping with the same quantity ledger (canonical units, mass↔volume density bridging). Planning uses **binary shortfall**: a key counts as needing to buy iff demand exceeds on-hand quantity after virtual consumption across the schedule; unquantified lines fall back to presence. `pantry restock <plan>` models **buy then cook**: add purchased packages, then deduct the plan’s full requirements, leaving packaging leftovers. Each plan may be restocked once.

### Planning algorithm (summary)

**Unconstrained:** multi-start greedy — for each possible first recipe, repeatedly append the unused candidate that adds the fewest new **to-buy** keys after quantity-aware pantry consumption (shared with shopping’s stock ledger); keep the schedule with the smallest net to-buy count. Partial stock does not fully exempt a key. Recipes are never repeated; if the pool is smaller than the requested slots, the plan is partial. Recipes whose estimate reports no calories, or calories with no protein/fat/carbs at all (e.g. an alcohol-only recipe), are dropped from the pool (not treated as meals).

**With nutrition bounds:** the selection is solved as an integer program over the **whole recipe pool** (no candidate cap) by [HiGHS](https://highs.dev/), with a two-phase lexicographic objective — first minimize total bound-violation magnitude (so a feasible plan is returned whenever one exists), then minimize the net to-buy count. When a solve can't be proven optimal within its time budget, the best feasible plan found so far is used; the greedy scheduler is the fallback only if the solver returns nothing usable. Recipes whose **ingredient coverage** falls below a threshold (default 90% of estimable ingredients resolved; `MIN_INGREDIENT_COVERAGE`) are excluded from the pool here, since an understated estimate can't be trusted against a constraint — run `nutrition fetch` to cover more ingredients and unlock more recipes. Unconstrained planning keeps them.

**With `--tod`:** each in-day meal index maps to breakfast, lunch, dinner, or any. Labels come from `meta.tags` and comma-split `meta.category` (`brunch` counts as breakfast and lunch, `supper` as dinner). Matching is **exact token** after title-key normalization (`dinner` / `supper` yes; `Dinners` or `Quick Dinner` no — use a dedicated tag or comma-split category). Unlabeled recipes fit only unrestricted slots. Enforcement is soft and ranks after nutrition magnitude (miss count, then net to-buy). The exact solver switches to per-slot variables (`pool × slots`, three lex phases) so assignment respects slot identity; on very large pools this is heavier than the flat model and more likely to time out to greedy. See module docs in `src/planning/mod.rs`, `src/planning/tod.rs`, and `src/planning/ilp.rs`.

### Nutrition bounds TOML

```toml
[per_day]
protein_g = { min = 50.0, max = 200.0 }
kcal = { max = 3000.0 }
# Target macro split as a share of total macro grams (protein_g + fat_g + carbs_g),
# within a ±tolerance band in percentage points (default 5). Config only.
ratio = { protein = 30, fat = 30, carb = 40 }
# ratio = { protein = 30, fat = 30, carb = 40, tolerance = 8 }

[per_meal]
protein_g = { min = 15.0 }

[plan]
protein_g = { min = 350.0 }

# Filter the candidate pool by the publisher's schema.org recipeCategory, to keep
# standalone meals and drop components (sauces, dressings, condiments).
[category]
whitelist = ["Main Course", "Dinner", "Entree"]
blacklist = ["Sauce", "Dressing", "Condiment", "Dip"]
```

CLI `--min-*` / `--max-*` flags overlay `per_day` min/max only. Nutrients: `kcal`, `protein_g`, `fat_g`, `carbs_g`. A `ratio` table (any scope, config only) targets a macro split by grams; a share is satisfied within its tolerance band, and deviation beyond the band (in grams) is reported as a best-effort violation and minimized by the solver.

The `[category]` table filters the candidate pool by `recipeCategory` (matched case-insensitively). The **blacklist** always excludes a matching recipe; a non-empty **whitelist** is **strict** — only recipes whose category is on it are eligible, so recipes with no category are excluded. The blacklist wins when a recipe matches both. Category filtering is independent of the macro bounds (a category-only config still uses the unconstrained planner). Categories are captured on import and can be backfilled across the existing DB with `smarter-recipes refresh --all --apply`.


### Purchase optimization (summary)

Requirements for a plan are reduced by pantry quantities first. Then enumerate bounded multisets of packages with total size ≥ required amount; rank by minimum cost, then minimum leftover, then fewer packages. See `src/shopping/mod.rs`.

## Development

```bash
cargo test
cargo fmt
cargo clippy --all-targets -- -D warnings
```

## Sample data

The `recipes/` directory includes breakfast and dinner examples that share ingredients (eggs, milk, butter, garlic, etc.) so `plan` and `shop` demonstrate overlap and packaging behavior.

## License

MIT
