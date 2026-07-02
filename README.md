# smarter-recipes

CLI tool that ingests recipes from multiple sources, stores them in a local SQLite database, plans meals to **minimize distinct ingredients** (no recipe repeats; fewer shopping-list line items), and builds **optimized shopping lists** that minimize cost then leftover waste.

## Features

| Area | What you get |
|------|----------------|
| **Ingestion** | JSON / TOML / plain text files, web pages (schema.org `Recipe` JSON-LD with HTML fallback), images via Tesseract OCR or `.txt` sidecars |
| **Normalization** | Free-text ingredient lines → name, quantity, unit; units converted to canonical g / ml / ea for aggregation |
| **Storage** | Embedded SQLite; ingredients deduplicated by `(name, unit kind)`; pantry stock by same identity |
| **Pantry** | Track on-hand ingredients; mark shopping results as purchased; plan and shop net of stock |
| **Planning** | Multi-start min-union scheduler, no recipe repeats; pantry keys not counted as “new” (documented in `src/planning/mod.rs`) |
| **Shopping** | Package multiset optimization: **cost first**, then **minimum leftover**; requirements reduced by pantry (documented in `src/shopping/mod.rs`) |
| **Extensibility** | New ingest sources implement `RecipeSourceIngest`; custom package catalogs via JSON overlay |

## Requirements

- **Rust** 1.74+ (edition 2021)
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

# Crawl a seed URL for same-host recipe pages (BFS). Works from a category page:
# links may point at site-root posts (not only path descendants of the seed).
# --depth N follows links from fetched pages up to N hops; --limit caps fetches;
# --jobs is concurrency. Category/nav pages are not remembered as failures, so
# re-runs can still walk them to find newly published recipes. Only hard fetch
# errors are persisted (skipped unless --retry-failed). Asset/author/tag URLs
# are deny-listed to save budget.
smarter-recipes scrape 'https://example.com/recipes' --limit 10 --jobs 8
smarter-recipes scrape 'https://example.com/category/chicken' --depth 3 --limit 50
smarter-recipes scrape 'https://example.com/recipes' --dry-run
smarter-recipes scrape 'https://example.com/recipes' --retry-failed

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

# Plan 5 days, 1 meal/day, minimize distinct ingredients (no repeats).
# On-hand pantry keys are treated as already covered when scoring plans.
smarter-recipes plan --days 5 --per-day 1

# Restrict the candidate pool
smarter-recipes plan --days 3 --pool <id1>,<id2>,<id3>

# Shopping list with package recommendations + leftover flags
# (amounts already in the pantry are subtracted / omitted)
smarter-recipes shop <plan-id-or-prefix>

# After buying, mark the plan's package totals as purchased (add to pantry)
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
  ingest/       Pluggable sources: file, url, ocr, crawl (index scraping)
  storage/      SQLite persistence + ingredient dedup + pantry stock
  planning/     Min-union meal planner (no repeats; pantry-aware)
  shopping/     Package purchase optimizer (nets against pantry)
  pricing/      Package catalog, density table, and store sources (Open Food Facts / fixture)
  cli/          clap commands
```

**Design choices**

1. **Canonical units** — Mass→g, volume→ml, count→ea. Only same `UnitKind` quantities are summed.
2. **Ingredient identity** — `(normalized_name, UnitKind)` so “2 cups milk” and “500 ml milk” aggregate when both are volume. Pantry rows use the same key.
3. **Core vs I/O** — Normalization, planning, and purchase optimization are pure and unit-tested without network or OCR.
4. **New ingest source** — Implement `RecipeSourceIngest` in `ingest/`, wire it in `ingest_from`.
5. **Density table** — Volume-measured dry goods (flour, sugar, salt, …) convert to mass for realistic packages (`src/pricing/density.rs`).
6. **Store sources** — `ProductSource` trait with Open Food Facts + fixture backends (`--fetch-prices`). Graceful fallback to the offline catalog.
7. **Pantry** — On-hand stock is optional input to planning (keys already covered) and shopping (quantities subtracted). `pantry restock <plan>` adds the shopping list’s *purchased* package totals after a trip.

### Planning algorithm (summary)

Multi-start greedy: for each possible first recipe, repeatedly append the unused candidate that adds the fewest new ingredient keys (relative to pantry + already selected); keep the schedule with the smallest **net** union (`|union − pantry|`). Recipes are never repeated; if the pool is smaller than the requested slots, the plan is partial. See module docs in `src/planning/mod.rs`.

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
