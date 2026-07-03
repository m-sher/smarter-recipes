# Search-scrape design

**Date:** 2026-07-03  
**Status:** approved

## Goal

Discover recipe pages via DuckDuckGo HTML search (user-supplied queries), then multi-host BFS-crawl result sites—without being limited to a single parent URL. Separate from existing same-host `scrape`. Success target for the first fill: ~1000 recipes in the local DB.

## Non-goals

- Other search engines or API keys
- Domain allowlists / curated site lists
- Changing single-URL `scrape` behavior
- Persistent rate-limit config files

## CLI

```text
smarter-recipes search-scrape "<query>" \
  [--limit N] [--jobs N] [--depth N] [--pages N] \
  [--dry-run] [--retry-failed]
```

| Arg / flag | Default | Meaning |
|------------|---------|---------|
| `query` | required | Free-text search string |
| `--limit` | 50 | Max non-search page fetches this run |
| `--jobs` | 8 | Concurrent page fetches |
| `--depth` | 2 | BFS depth from each result URL (same-host expansion) |
| `--pages` | 2 | DuckDuckGo HTML result pages to load for the query |
| `--dry-run` | false | Discover/report; do not save |
| `--retry-failed` | false | Include URLs previously recorded as hard fetch failures |

Skip set and save-time identity match `scrape`: known source URLs (+ hard failures unless retry); skip save on URL or normalized title collision.

## Pipeline

1. **Search** — For each result page `0..pages-1`, fetch DuckDuckGo HTML (`https://html.duckduckgo.com/html/?q=…` with offset). Parse organic result anchors; unwrap `uddg=` redirect targets; normalize and dedupe. Search fetches do **not** count toward `--limit`.
2. **Seed frontier** — Each unique result URL is a seed **fetch target** (unlike site `scrape`, which only uses the seed for link discovery). Seeds may be recipe pages or listings.
3. **Multi-seed BFS** — Shared queue, `enqueued` set, `skip` set, and fetch budget across all hosts. When expanding links from a fetched page, scope to **same host as that page** (not the search engine). Reuse listing/deny/ingest policy from `crawl.rs`.
4. **Persist** — Same as `scrape`: save new recipes; record only hard fetch failures; clear failure on later success.

## Components

| Piece | Location |
|-------|----------|
| DDG query + result URL extraction | `src/ingest/search.rs` |
| Multi-seed BFS (`scrape_from_seeds`) | `src/ingest/crawl.rs` |
| `search_scrape_recipes` orchestration | `src/ingest/search.rs` or crawl |
| CLI | `src/cli/mod.rs` (`Commands::SearchScrape`) |
| README | short usage note under scrape section |

`HtmlFetcher` remains the test seam (MapFetcher fixtures for DDG HTML + multi-host pages).

## Error handling

- Search page hard-fail: abort the run with context (no silent empty success if the engine is unreachable on page 0).
- Empty results after successful search HTML: return empty outcome (candidates 0).
- Per-recipe/page failures: same as scrape (progress events; hard vs not-recipe).

## Testing

- Unit: parse fixed DDG HTML fixture → expected absolute recipe URLs (including `uddg` unwrap).
- Unit: multi-seed BFS across two hosts with MapFetcher; listings expand; deny-list holds; limit shared.
- Live (manual / agent): multiple meal-oriented queries until `status` shows ≥ ~1000 recipes.

## Fill queries (initial set)

Examples: `best meal prep`, `breakfast recipes`, `high protein dinners`, `easy lunch recipes`, `healthy snack recipes`, `vegetarian dinner`, `sheet pan dinner`, `air fryer recipes`, etc. Re-run with higher `--limit` as needed; dedup skips already-known URLs/titles.
