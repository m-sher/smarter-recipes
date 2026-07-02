Build a CLI tool in Rust that ingests recipes from multiple sources, stores them in a database, and plans meals across a number of days to minimize shopping trips for me and minimizes leftover waste.

This is a pretty large task, so work incrementally in separate commits with concise commit messages. Lay out the architecture first so features can be added independently.

Here are some of the primary goals:

1. Recipe ingestion from multiple sources to produce a normalized recipe (title, servings, ingredients with quantity + unit, steps, metadata, etc.):
   - OCR from images of recipes / ingredient lists
   - Fetching recipes from web pages. Prefer schema.org `Recipe` JSON-LD when present, fall back to heuristic HTML parsing.
   - Manual input (e.g. a JSON or TOML file, or interactive entry).
   - Also make sure that adding a new source later is straightforward.

2. Parse free-text ingredient lines into name, quantity, and unit. Normalize units (mass, volume, count) so quantities of the same ingredient can be summed across recipes. Handle the messy cases gracefully and keep the original text.
   - This is a big one and most of the functionality relies on it so invest heavily here and make sure to test it very well.

3. Persist recipes, ingredients, and plans in a local embedded database (don't reinvent). Make sure to dedup ingredients so quantities can be aggregated.

4. Given a number of days, recipes per day, and the candidate recipe pool, produce a plan that orders/selects recipes to maximize ingredient overlap, so ingredients bought for early recipes are reused later. Make sure to explain the algorithm in comments/docs.

5. For the shopping list, determine purchasable package sizes per ingredient and choose quantities that cover the required amount while minimizing cost first, then minimizing leftover. Examples: 14oz milk needed, available as 16oz or 32oz -> prefer the one with less extra (16oz, 2oz leftover) and flag the leftover amount; 32oz milk needed, same options available -> prefer whichever of 2x16oz or 32oz has the lowest cost (since both have 0 extra).
   - Pull package sizes/prices from store websites. Use public APIs where available, otherwise, just best-effort scraping.

6. CLI surface
   - `import <source> <input>` - ingest a recipe from an image/URL/file.
   - `list`/`show <id>` - browse stored recipes.
   - `plan --days N --per-day M [--pool ...]` - generate a meal plan.
   - `shop <plan>` - produce the optimized shopping list with package recommendations and flagged leftovers.

7. Engineering expectations
   - Rust w/ format linter 
   - Keep the core logic (normalization, planning, purchase optimization) testable offline, independent of network/OCR/scraping.
   - Unit tests for the parts where correctness matters most: ingredient/unit normalization, overlap planning, and package optimization.
   - A README.md covering setup (including any system deps), usage, and architecture.
   - Sensible module boundaries; document non-obvious decisions inline.
   - Keep comments concise and describe only current behavior - don't write comments to describe the decision process, debugging, or any previous behavior.

Start by initializing this repo, then scaffold the project and the domain model, then build outward: ingestion -> normalization -> storage -> planning -> purchase optimization. There is no time limit here. We need a high quality, well-tested product, so when you encounter difficulties, take no shortcuts.
