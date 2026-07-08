import type {
  Api,
  DbStatus,
  PantryItemView,
  PlanSummary,
  PlanView,
  RecipeDetail,
  RecipeSummary,
  ShopItemView,
} from "./bridge";

export type Page = "home" | "library" | "pantry" | "plan" | "recipe";

export type AppState = {
  page: Page;
  status: DbStatus | null;
  recipes: RecipeSummary[];
  pantry: PantryItemView[];
  plans: PlanSummary[];
  activePlan: PlanView | null;
  shop: ShopItemView[];
  recipeDetail: RecipeDetail | null;
  planDays: number;
  planMealsPerDay: number;
  planTod: boolean;
  planSave: boolean;
  pantryLine: string;
  libraryFilter: string;
  error: string | null;
  notice: string | null;
  loading: boolean;
  busy: boolean;
};

export type Handlers = {
  onNav: (p: Page) => void;
  onOpenRecipe: (id: string) => void;
  onPlanDays: (n: number) => void;
  onPlanMeals: (n: number) => void;
  onPlanTod: (v: boolean) => void;
  onPlanSave: (v: boolean) => void;
  onCreatePlan: () => void;
  onOpenPlan: (id: string) => void;
  onShop: () => void;
  onPantryLine: (v: string) => void;
  onPantryAdd: () => void;
  onPantryRemove: (name: string, kind: string) => void;
  onLibraryFilter: (v: string) => void;
  onLibrarySearch: () => void;
};

export function initialState(): AppState {
  return {
    page: "home",
    status: null,
    recipes: [],
    pantry: [],
    plans: [],
    activePlan: null,
    shop: [],
    recipeDetail: null,
    planDays: 3,
    planMealsPerDay: 1,
    planTod: false,
    planSave: true,
    pantryLine: "",
    libraryFilter: "",
    error: null,
    notice: null,
    loading: true,
    busy: false,
  };
}

function shortId(id: string): string {
  return id.slice(0, 8);
}

export function render(root: HTMLElement, state: AppState, h: Handlers): void {
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
    ["plan", "Plan"],
  ] as const) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.dataset.page = page;
    if (state.page === page || (state.page === "recipe" && page === "library")) {
      b.classList.add("active");
    }
    b.addEventListener("click", () => h.onNav(page));
    nav.append(b);
  }
  sidebar.append(nav);
  shell.append(sidebar);

  const main = el("main", "main");
  if (state.error) main.append(el("div", "error", state.error));
  if (state.notice) main.append(el("div", "notice", state.notice));
  if (state.busy) main.append(el("div", "empty", "Working…"));

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
    const bar = el("div", "toolbar");
    const input = document.createElement("input");
    input.type = "search";
    input.placeholder = "Filter titles…";
    input.value = state.libraryFilter;
    input.className = "input";
    input.addEventListener("input", () => h.onLibraryFilter(input.value));
    input.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter") h.onLibrarySearch();
    });
    const btn = button("Search", () => h.onLibrarySearch());
    bar.append(input, btn);
    main.append(bar);

    if (state.loading && state.recipes.length === 0) {
      main.append(el("div", "empty", "Loading…"));
    } else if (state.recipes.length === 0) {
      main.append(el("div", "empty", "No recipes match."));
    } else {
      const list = el("ul", "list");
      for (const r of state.recipes) {
        const li = document.createElement("li");
        li.classList.add("clickable");
        const left = document.createElement("div");
        left.append(
          el("div", "title", r.title),
          el("div", "sub", `${shortId(r.id)} · ${r.ingredient_count} ingredient line(s)`),
        );
        li.append(left);
        if (r.category) li.append(el("span", "badge", r.category));
        li.addEventListener("click", () => h.onOpenRecipe(r.id));
        list.append(li);
      }
      main.append(list);
    }
  } else if (state.page === "recipe") {
    const d = state.recipeDetail;
    if (!d) {
      main.append(el("div", "empty", "Loading recipe…"));
    } else {
      main.append(pageHeader(d.title, shortId(d.id)));
      const meta = el("div", "card");
      const bits = [
        d.category ? `Category: ${d.category}` : "Uncategorized",
        d.servings != null ? `Servings: ${d.servings}` : null,
        `Source: ${d.source}`,
      ].filter(Boolean) as string[];
      meta.append(el("p", "muted", bits.join(" · ")));
      main.append(meta);

      const ing = el("div", "card");
      ing.append(el("h3", "", "Ingredients"));
      const ul = el("ul", "plain-list");
      for (const line of d.ingredients) {
        const li = document.createElement("li");
        li.textContent = line;
        ul.append(li);
      }
      ing.append(ul);
      main.append(ing);

      if (d.steps.length) {
        const st = el("div", "card");
        st.append(el("h3", "", "Steps"));
        const ol = document.createElement("ol");
        ol.className = "plain-list numbered";
        for (const step of d.steps) {
          const li = document.createElement("li");
          li.textContent = step;
          ol.append(li);
        }
        st.append(ol);
        main.append(st);
      }

      main.append(button("← Back to library", () => h.onNav("library"), "ghost"));
    }
  } else if (state.page === "pantry") {
    main.append(pageHeader("Pantry", `${state.pantry.length} item(s)`));
    const form = el("div", "toolbar");
    const input = document.createElement("input");
    input.type = "text";
    input.placeholder = 'Add stock, e.g. "2 cups milk"';
    input.value = state.pantryLine;
    input.className = "input grow";
    input.addEventListener("input", () => h.onPantryLine(input.value));
    input.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter") h.onPantryAdd();
    });
    form.append(input, button("Add", () => h.onPantryAdd(), "primary"));
    main.append(form);

    if (state.loading && state.pantry.length === 0) {
      main.append(el("div", "empty", "Loading…"));
    } else if (state.pantry.length === 0) {
      main.append(el("div", "empty", "Pantry is empty."));
    } else {
      const list = el("ul", "list");
      for (const p of state.pantry) {
        const li = document.createElement("li");
        const left = document.createElement("div");
        left.append(el("div", "title", p.name), el("div", "sub", p.kind));
        li.append(left);
        const right = el("div", "row-actions");
        right.append(el("span", "badge", `${formatQty(p.quantity_canonical)} ${p.unit_label}`));
        const rm = button("Remove", () => h.onPantryRemove(p.name, p.kind), "danger small");
        right.append(rm);
        li.append(right);
        list.append(li);
      }
      main.append(list);
    }
  } else if (state.page === "plan") {
    main.append(pageHeader("Plan", state.activePlan ? shortId(state.activePlan.id) : "new"));

    const form = el("div", "card form-grid");
    form.append(el("h3", "", "Generate meal plan"));
    form.append(
      labeledNumber("Days", state.planDays, (n) => h.onPlanDays(n)),
      labeledNumber("Meals / day", state.planMealsPerDay, (n) => h.onPlanMeals(n)),
    );
    const tod = labeledCheck("Time-of-day steering", state.planTod, (v) => h.onPlanTod(v));
    const save = labeledCheck("Save plan to database", state.planSave, (v) => h.onPlanSave(v));
    form.append(tod, save);
    form.append(button(state.busy ? "Planning…" : "Create plan", () => h.onCreatePlan(), "primary"));
    main.append(form);

    if (state.plans.length) {
      const saved = el("div", "card");
      saved.append(el("h3", "", "Saved plans"));
      const list = el("ul", "list");
      for (const p of state.plans) {
        const li = document.createElement("li");
        li.classList.add("clickable");
        const left = document.createElement("div");
        left.append(
          el("div", "title", shortId(p.id)),
          el("div", "sub", `${p.days}d × ${p.meals_per_day} · ${p.meal_count} meals`),
        );
        li.append(left);
        li.addEventListener("click", () => h.onOpenPlan(p.id));
        list.append(li);
      }
      saved.append(list);
      main.append(saved);
    }

    if (state.activePlan) {
      const plan = state.activePlan;
      const card = el("div", "card");
      card.append(
        el("h3", "", `Plan ${shortId(plan.id)}`),
        el("p", "muted", `${plan.days} day(s) · ${plan.meals_per_day} meal(s)/day · ${plan.meals.length} scheduled`),
      );

      // Group meals by day
      const byDay = new Map<number, typeof plan.meals>();
      for (const m of plan.meals) {
        const arr = byDay.get(m.day) ?? [];
        arr.push(m);
        byDay.set(m.day, arr);
      }
      for (const [day, meals] of [...byDay.entries()].sort((a, b) => a[0] - b[0])) {
        card.append(el("div", "day-label", `Day ${day + 1}`));
        const ul = el("ul", "list compact");
        for (const m of meals) {
          const li = document.createElement("li");
          const left = document.createElement("div");
          const title = m.uses_pantry ? `${m.recipe_title} ★` : m.recipe_title;
          left.append(
            el("div", "title", title),
            el("div", "sub", `meal ${m.meal + 1} · ${shortId(m.recipe_id)}`),
          );
          li.append(left);
          if (m.uses_pantry) li.append(el("span", "badge", "pantry"));
          li.classList.add("clickable");
          li.addEventListener("click", () => h.onOpenRecipe(m.recipe_id));
          ul.append(li);
        }
        card.append(ul);
      }

      const rat = el("pre", "rationale");
      rat.textContent = plan.rationale;
      card.append(el("h3", "", "Rationale"), rat);
      card.append(button("Shopping list", () => h.onShop(), "primary"));
      main.append(card);
    }

    if (state.shop.length) {
      const shop = el("div", "card");
      shop.append(el("h3", "", "Shopping list"));
      const ul = el("ul", "list");
      for (const item of state.shop) {
        const li = document.createElement("li");
        const left = document.createElement("div");
        left.append(
          el("div", "title", item.name),
          el("div", "sub", `need ${formatQty(item.need)} ${item.unit}`),
        );
        li.append(left);
        if (item.leftover > 0) {
          li.append(el("span", "badge", `leftover ${formatQty(item.leftover)}`));
        }
        ul.append(li);
      }
      shop.append(ul);
      main.append(shop);
    }
  }

  shell.append(main);
  root.append(shell);
}

export async function loadPageData(api: Api, page: Page, state: AppState): Promise<Partial<AppState>> {
  try {
    if (page === "home") {
      const status = await api.getStatus();
      return { status, error: null, loading: false };
    }
    if (page === "library") {
      const recipes = await api.listRecipes(state.libraryFilter || null);
      return { recipes, error: null, loading: false };
    }
    if (page === "pantry") {
      const pantry = await api.listPantry();
      return { pantry, error: null, loading: false };
    }
    if (page === "plan") {
      const plans = await api.listPlans();
      return { plans, error: null, loading: false };
    }
    return { loading: false };
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

function button(label: string, onClick: () => void, variant = ""): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.textContent = label;
  b.className = ["btn", variant].filter(Boolean).join(" ");
  b.addEventListener("click", (ev) => {
    ev.stopPropagation();
    onClick();
  });
  return b;
}

function labeledNumber(label: string, value: number, onChange: (n: number) => void): HTMLElement {
  const wrap = el("label", "field");
  wrap.append(el("span", "", label));
  const input = document.createElement("input");
  input.type = "number";
  input.min = "1";
  input.value = String(value);
  input.className = "input";
  input.addEventListener("change", () => {
    const n = Math.max(1, Number(input.value) || 1);
    onChange(n);
  });
  wrap.append(input);
  return wrap;
}

function labeledCheck(label: string, value: boolean, onChange: (v: boolean) => void): HTMLElement {
  const wrap = el("label", "field check");
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = value;
  input.addEventListener("change", () => onChange(input.checked));
  wrap.append(input, el("span", "", label));
  return wrap;
}
