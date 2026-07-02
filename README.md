# smarter-recipes

CLI tool that ingests recipes from multiple sources, stores them in a local SQLite database, plans meals to **maximize ingredient overlap** (fewer shopping trips), and builds **optimized shopping lists** that minimize cost then leftover waste.

## Features

| Area | What you get |
|------|----------------|
| **Ingestion** | JSON / TOML / plain text files, web pages (schema.org `Recipe` JSON-LD with HTML fallback), images via Tesseract OCR or `.txt` sidecars |
| **Normalization** | Free-text ingredient lines → name, quantity, unit; units converted to canonical g / ml / ea for aggregation |
| **Storage** | Embedded SQLite; ingredients deduplicated by `(name, unit kind)` |
| **Planning** | Greedy overlap scheduler with recency window (documented in `src/planning/mod.rs`) |
| **Shopping** | Package multiset optimization: **cost first**, then **minimum leftover** (documented in `src/shopping/mod.rs`) |
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

# Crawl an index page for recipe pages and import new ones.
# Finds same-site links below the given path (e.g. .../recipes/<dish>),
# fetches candidates concurrently (--jobs) with live progress, and imports the
# ones that parse as recipes. URLs already imported — or previously recorded as
# failures — are skipped without re-fetching, so re-running only pulls in new
# recipes. --limit caps how many new pages to fetch this run (not how many parse
# successfully); use --retry-failed to re-attempt known failures.
smarter-recipes scrape 'https://example.com/recipes' --limit 10 --jobs 8
smarter-recipes scrape 'https://example.com/recipes' --dry-run        # preview only
smarter-recipes scrape 'https://example.com/recipes' --retry-failed   # retry past failures

# Browse
smarter-recipes list
smarter-recipes list --filter pasta
smarter-recipes show <id-or-prefix>
smarter-recipes status

# Plan 5 days, 1 meal/day, maximize ingredient reuse
smarter-recipes plan --days 5 --per-day 1

# Restrict the candidate pool
smarter-recipes plan --days 3 --pool <id1>,<id2>,<id3>

# Shopping list with package recommendations + leftover flags
smarter-recipes shop <plan-id-or-prefix>

# Show how plan ordering introduces ingredients (trip analysis)
smarter-recipes shop <plan-id> --trips

# Enrich catalog from Open Food Facts (network) or a recorded fixture
smarter-recipes shop <plan-id> --fetch-prices openfoodfacts
smarter-recipes shop <plan-id> --fetch-prices fixture --store-fixture fixtures/store_catalog.json

# Custom package sizes/prices (JSON object: name → [packages])
smarter-recipes shop <plan-id> --catalog my_catalog.json

# Export / delete / reparse
smarter-recipes export <id> -o out.json
smarter-recipes delete <id>
smarter-recipes reparse <id>
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
  domain/       Shared types: Recipe, IngredientLine, MealPlan, units
  normalize/    Ingredient parsing + unit tables (no I/O)
  ingest/       Pluggable sources: file, url, ocr, crawl (index scraping)
  storage/      SQLite persistence + ingredient dedup
  planning/     Overlap-maximizing meal planner
  shopping/     Package purchase optimizer
  pricing/      Offline package catalog (+ scrape extension point)
  cli/          clap commands
```

**Design choices**

1. **Canonical units** — Mass→g, volume→ml, count→ea. Only same `UnitKind` quantities are summed.
2. **Ingredient identity** — `(normalized_name, UnitKind)` so “2 cups milk” and “500 ml milk” aggregate when both are volume.
3. **Core vs I/O** — Normalization, planning, and purchase optimization are pure and unit-tested without network or OCR.
4. **New ingest source** — Implement `RecipeSourceIngest` in `ingest/`, wire it in `ingest_from`.
5. **Density table** — Volume-measured dry goods (flour, sugar, salt, …) convert to mass for realistic packages (`src/pricing/density.rs`).
6. **Store sources** — `ProductSource` trait with Open Food Facts + fixture backends (`--fetch-prices`). Graceful fallback to the offline catalog.

### Planning algorithm (summary)

Seed with the recipe that shares the most ingredients with the rest of the pool, then greedily append the candidate maximizing weighted overlap with already covered ingredients plus a short recency window. See module docs in `src/planning/mod.rs`.

### Purchase optimization (summary)

Enumerate bounded multisets of packages with total size ≥ required amount; rank by minimum cost, then minimum leftover, then fewer packages. See `src/shopping/mod.rs`.

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
