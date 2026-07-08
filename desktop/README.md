# Smarter Recipes Desktop

Tauri 2 + Vite desktop shell over the existing `smarter_recipes` library and SQLite database.

## Prerequisites

- Rust (same as the CLI crate)
- Node.js 20+
- Linux: WebKitGTK for Tauri (`libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, etc. — see [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/))

## Develop

```bash
cd desktop
npm install
npm run tauri dev
```

The UI loads from Vite (`localhost:1420`) and calls Tauri commands that open the **same default DB** as the CLI (`SMARTER_RECIPES_DB` / platform data dir).

## Visual regression tests

The UI is testable **without** launching Tauri. Playwright starts Vite, forces mock data (`?mock=1` / `__SR_MOCK__`), captures full-page frames, and compares them to golden PNGs.

```bash
cd desktop
npm install
npx playwright install chromium   # once
npm run test:visual               # compare to baselines
npm run test:visual:update        # rewrite golden frames after intentional UI changes
```

Snapshots live under `tests/visual/shell.spec.ts-snapshots/`.

Mock fixtures are deterministic (fixed recipe/pantry rows) so diffs reflect design changes, not live DB contents.

## Commands exposed (v1)

| Command | Purpose |
|---------|---------|
| `get_status` | DB path + recipe/plan/pantry counts |
| `list_recipes` | Optional title filter |
| `list_pantry` | On-hand stock |

## Layout

```
desktop/
  src/                 # Vite UI (TypeScript)
  src-tauri/           # Tauri host, invokes smarter_recipes
  tests/visual/        # Playwright frame comparisons
```
