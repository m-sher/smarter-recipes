# Smarter Recipes Desktop

Tauri 2 + Vite desktop shell over the existing `smarter_recipes` library and SQLite database.

## Features (v1)

| Page | Actions |
|------|---------|
| **Home** | DB path, recipe / plan / pantry counts |
| **Library** | Search, open recipe detail, delete |
| **Pantry** | List, add free-text stock, remove |
| **Plan** | Days/meals/TOD, optional nutrition TOML + protein/kcal, create & save, open saved, ★ pantry meals, shop list, restock |
| **Import** | `auto` / `file` / `url` / `epub` ingest (same pipeline as CLI) |

## Prerequisites

- Rust (same as the CLI crate)
- Node.js 20+
- Linux: WebKitGTK/GTK for native window — see [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)

## Develop

```bash
cd desktop
npm install
npm run tauri dev
```

Uses the **same default DB** as the CLI (`SMARTER_RECIPES_DB` / platform data dir).

## Visual regression tests

UI is tested **without** launching Tauri. Playwright boots Vite with mock data and compares full-page frames to golden PNGs.

```bash
cd desktop
npx playwright install chromium   # once
npm run test:visual               # compare
npm run test:visual:update        # rewrite goldens after intentional UI changes
```

Snapshots: `tests/visual/shell.spec.ts-snapshots/`.

## Layout

```
desktop/
  src/                 # Vite UI
  src-tauri/           # Tauri host → smarter_recipes
  tests/visual/        # Playwright frame comparisons
```
