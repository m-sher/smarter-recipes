# smarter-recipes — project conventions

## GitHub: use MCP tools, not the `gh` CLI

For issues, PRs, reviews, comments, checks, merges, and any other GitHub
operations, use the **GitHub MCP** (`search_tool` → `use_tool` with
`github__…`). Do not use `gh` unless MCP is unavailable or the user asks for it.

## Bulk database writes: batch in a transaction, never per-row autocommit

`rusqlite` runs each `conn.execute` as its own autocommitted transaction, and
every commit fsyncs. Saving thousands of rows one statement at a time therefore
pays one disk sync per statement — it is I/O-bound, not CPU-bound, and runs
50–100× slower than necessary regardless of platform. (Reparsing ~8.8k recipes
took 24+ min per-row vs ~2.3 s batched.)

Rule: any code path that writes many rows must wrap the writes in a single
transaction (or bounded chunks), so it fsyncs once per batch, not once per row.
Use `Store::save_recipes` for bulk recipe writes; for other bulk writes wrap the
loop in `BEGIN`/`COMMIT` (`ROLLBACK` on error), chunked at ~500.

Do **not** reach for threads to speed up writes: SQLite allows a single writer,
so concurrent write threads only contend on the lock. The win is batching the
commits, not parallelizing them. (Diagnose a slow write path by its CPU time —
near-zero CPU with long wall-clock means it is fsync-bound, so batch it.)

Long bulk operations must print progress (per committed chunk), so they are
visibly making forward progress rather than looking hung.

Known callers still on per-row autocommit — migrate to the batched path when
touched: `apply_scrape_outcome` (import) and `refresh_recipes` in `src/cli/mod.rs`.
