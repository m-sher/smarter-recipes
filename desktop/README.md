# Smarter Recipes Desktop

Tauri 2 + Vite desktop shell over the existing `smarter_recipes` library and SQLite database.

## Features (v1)

| Page | Actions |
|------|---------|
| **Home** | DB path, recipe / plan / pantry counts |
| **Library** | Search, open recipe detail, delete |
| **Pantry** | List, add free-text stock, remove |
| **Plan** | Result above options; days/meals/time-of-day/save; nutrition bounds editor (per day/meal/plan + ratio, category filter); load/save bounds file; per-day overrides; recipe pool; create & open saved; ★ pantry meals; shop; restock |
| **Import** | `auto` / `file` / `url` / `epub` ingest (same pipeline as CLI) |

## Prerequisites

- Rust (same as the CLI crate)
- Node.js 20+
- **Linux system libraries** (GTK 3 + WebKitGTK 4.1) — see below

### Linux deps (Ubuntu/Debian)

**Preferred (system-wide):**

```bash
sudo apt update
sudo apt install -y \
  build-essential curl wget file pkg-config \
  libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev patchelf \
  libssl-dev
```

**Without sudo:** a user-local sysroot can be used. If present at
`~/.local/tauri-sysroot`, source the helper before any Tauri build:

```bash
source desktop/env-linux.sh
```

## Develop

```bash
cd desktop
npm install
# if using user-local GTK/WebKit sysroot:
# source ./env-linux.sh
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
