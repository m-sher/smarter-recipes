import type { Api, DbStatus, PantryItemView, RecipeSummary } from "./bridge";

export type Page = "home" | "library" | "pantry";

export type AppState = {
  page: Page;
  status: DbStatus | null;
  recipes: RecipeSummary[];
  pantry: PantryItemView[];
  error: string | null;
  loading: boolean;
};

export function initialState(): AppState {
  return {
    page: "home",
    status: null,
    recipes: [],
    pantry: [],
    error: null,
    loading: true,
  };
}

function shortId(id: string): string {
  return id.slice(0, 8);
}

export function render(root: HTMLElement, state: AppState, onNav: (p: Page) => void): void {
  root.innerHTML = "";
  const shell = el("div", "app-shell");

  const sidebar = el("aside", "sidebar");
  const brand = el("div", "brand");
  brand.append(el("h1", "", "Smarter Recipes"), el("p", "", "Local meal planning"));
  sidebar.append(brand);

  const nav = el("nav", "nav");
  for (const [page, label] of [
    ["home", "Home"],
    ["library", "Library"],
    ["pantry", "Pantry"],
  ] as const) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.dataset.page = page;
    if (state.page === page) b.classList.add("active");
    b.addEventListener("click", () => onNav(page));
    nav.append(b);
  }
  sidebar.append(nav);
  shell.append(sidebar);

  const main = el("main", "main");
  if (state.error) {
    main.append(el("div", "error", state.error));
  }

  if (state.page === "home") {
    main.append(pageHeader("Home", state.status ? state.status.path : "…"));
    if (state.loading && !state.status) {
      main.append(el("div", "empty", "Loading…"));
    } else if (state.status) {
      const grid = el("div", "stat-grid");
      grid.append(
        statCard("Recipes", String(state.status.recipe_count)),
        statCard("Plans", String(state.status.plan_count)),
        statCard("Pantry items", String(state.status.pantry_count)),
      );
      main.append(grid);
      const card = el("div", "card");
      card.append(el("h3", "", "Database"), el("p", "muted", state.status.path));
      main.append(card);
    }
  } else if (state.page === "library") {
    main.append(pageHeader("Library", `${state.recipes.length} recipe(s)`));
    if (state.loading && state.recipes.length === 0) {
      main.append(el("div", "empty", "Loading…"));
    } else if (state.recipes.length === 0) {
      main.append(el("div", "empty", "No recipes yet — import via the CLI."));
    } else {
      const list = el("ul", "list");
      for (const r of state.recipes) {
        const li = document.createElement("li");
        const left = document.createElement("div");
        left.append(
          el("div", "title", r.title),
          el("div", "sub", `${shortId(r.id)} · ${r.ingredient_count} ingredient line(s)`),
        );
        li.append(left);
        if (r.category) li.append(el("span", "badge", r.category));
        list.append(li);
      }
      main.append(list);
    }
  } else if (state.page === "pantry") {
    main.append(pageHeader("Pantry", `${state.pantry.length} item(s)`));
    if (state.loading && state.pantry.length === 0) {
      main.append(el("div", "empty", "Loading…"));
    } else if (state.pantry.length === 0) {
      main.append(el("div", "empty", "Pantry is empty — add stock via the CLI or upcoming UI."));
    } else {
      const list = el("ul", "list");
      for (const p of state.pantry) {
        const li = document.createElement("li");
        const left = document.createElement("div");
        left.append(el("div", "title", p.name), el("div", "sub", p.kind));
        li.append(left);
        li.append(el("span", "badge", `${formatQty(p.quantity_canonical)} ${p.unit_label}`));
        list.append(li);
      }
      main.append(list);
    }
  }

  shell.append(main);
  root.append(shell);
}

export async function loadPageData(api: Api, page: Page): Promise<Partial<AppState>> {
  try {
    if (page === "home") {
      const status = await api.getStatus();
      return { status, error: null, loading: false };
    }
    if (page === "library") {
      const recipes = await api.listRecipes(null);
      return { recipes, error: null, loading: false };
    }
    const pantry = await api.listPantry();
    return { pantry, error: null, loading: false };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { error: msg, loading: false };
  }
}

function pageHeader(title: string, meta: string): HTMLElement {
  const h = el("div", "page-header");
  h.append(el("h2", "", title), el("div", "meta", meta));
  return h;
}

function statCard(label: string, value: string): HTMLElement {
  const s = el("div", "stat");
  s.append(el("div", "label", label), el("div", "value", value));
  return s;
}

function formatQty(n: number): string {
  if (Number.isInteger(n)) return String(n);
  return n.toFixed(1);
}

function el(tag: string, className = "", text?: string): HTMLElement {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
