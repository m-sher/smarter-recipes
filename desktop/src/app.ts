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
import { boundsForm, emptyBounds, type BoundsForm } from "./nutrition-form";
import type { NutritionBounds } from "./bridge";

export type Page = "home" | "library" | "pantry" | "plan" | "recipe" | "import";

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
  nutritionConfig: string;
  nutritionBounds: NutritionBounds;
  showBoundsEditor: boolean;
  minProtein: string;
  maxKcal: string;
  minKcal: string;
  maxProtein: string;
  minFat: string;
  maxFat: string;
  minCarbs: string;
  maxCarbs: string;
  /** Comma-separated recipe id prefixes (CLI --pool). Empty = all. */
  pool: string;
  pantryLine: string;
  libraryFilter: string;
  importSource: string;
  importInput: string;
  error: string | null;
  notice: string | null;
  loading: boolean;
  busy: boolean;
};

export type Handlers = {
  onNav: (p: Page) => void;
  onOpenRecipe: (id: string) => void;
  onDeleteRecipe: () => void;
  onPlanDays: (n: number) => void;
  onPlanMeals: (n: number) => void;
  onPlanTod: (v: boolean) => void;
  onPlanSave: (v: boolean) => void;
  onNutritionConfig: (v: string) => void;
  onLoadNutritionConfig: () => void;
  onSaveNutritionConfig: () => void;
  onMinKcal: (v: string) => void;
  onMaxKcal: (v: string) => void;
  onMinProtein: (v: string) => void;
  onMaxProtein: (v: string) => void;
  onMinFat: (v: string) => void;
  onMaxFat: (v: string) => void;
  onMinCarbs: (v: string) => void;
  onMaxCarbs: (v: string) => void;
  onPool: (v: string) => void;
  onReadBounds: () => NutritionBounds;
  onCreatePlan: () => void;
  onOpenPlan: (id: string) => void;
  onShop: () => void;
  onRestock: () => void;
  onPantryLine: (v: string) => void;
  onPantryAdd: () => void;
  onPantryRemove: (name: string, kind: string) => void;
  onLibraryFilter: (v: string) => void;
  onLibrarySearch: () => void;
  onImportSource: (v: string) => void;
  onImportInput: (v: string) => void;
  onImport: () => void;
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
    nutritionConfig: "",
    nutritionBounds: emptyBounds(),
    showBoundsEditor: true,
    minProtein: "",
    maxKcal: "",
    minKcal: "",
    maxProtein: "",
    minFat: "",
    maxFat: "",
    minCarbs: "",
    maxCarbs: "",
    pool: "",
    pantryLine: "",
    libraryFilter: "",
    importSource: "auto",
    importInput: "",
    error: null,
    notice: null,
    loading: true,
    busy: false,
  };
}

function shortId(id: string): string {
  return id.slice(0, 8);
}

/** Survives re-renders so focus is not lost while editing bounds. */
let planBoundsForm: BoundsForm | null = null;

export function clearPlanBoundsForm(): void {
  planBoundsForm = null;
}

export function ensurePlanBoundsForm(initial: NutritionBounds): BoundsForm {
  if (!planBoundsForm) {
    planBoundsForm = boundsForm(initial, () => {});
  }
  return planBoundsForm;
}

export function setPlanBoundsForm(b: NutritionBounds): void {
  ensurePlanBoundsForm(b).set(b);
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
    ["import", "Import"],
  ] as const) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    if (state.page === page || (state.page === "recipe" && page === "library")) b.classList.add("active");
    b.addEventListener("click", () => h.onNav(page));
    nav.append(b);
  }
  sidebar.append(nav);
  shell.append(sidebar);

  const main = el("main", "main");
  if (state.error) main.append(el("div", "error", state.error));
  if (state.notice) main.append(el("div", "notice", state.notice));
  if (state.busy) main.append(el("div", "empty", "Working…"));

  if (state.page === "home") renderHome(main, state);
  else if (state.page === "library") renderLibrary(main, state, h);
  else if (state.page === "recipe") renderRecipe(main, state, h);
  else if (state.page === "pantry") renderPantry(main, state, h);
  else if (state.page === "plan") renderPlan(main, state, h);
  else if (state.page === "import") renderImport(main, state, h);

  shell.append(main);
  root.append(shell);
}

function renderHome(main: HTMLElement, state: AppState): void {
  main.append(pageHeader("Home", state.status ? state.status.path : "…"));
  if (state.loading && !state.status) {
    main.append(el("div", "empty", "Loading…"));
    return;
  }
  if (!state.status) return;
  const grid = el("div", "stat-grid");
  grid.append(
    statCard("Recipes", String(state.status.recipe_count)),
    statCard("Plans", String(state.status.plan_count)),
    statCard("Pantry items", String(state.status.pantry_count)),
  );
  main.append(grid);
  const card = el("div", "card");
  card.append(el("h3", "", "Database"), el("p", "muted", state.status.path));
  card.append(
    el(
      "p",
      "muted",
      "Import recipes, stock the pantry, then generate a plan. Same SQLite DB as the CLI.",
    ),
  );
  main.append(card);
}

function renderLibrary(main: HTMLElement, state: AppState, h: Handlers): void {
  main.append(pageHeader("Library", `${state.recipes.length} recipe(s)`));
  const bar = el("div", "toolbar");
  const input = document.createElement("input");
  input.type = "search";
  input.placeholder = "Filter titles…";
  input.value = state.libraryFilter;
  input.className = "input grow";
  input.addEventListener("input", () => h.onLibraryFilter(input.value));
  input.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") h.onLibrarySearch();
  });
  bar.append(input, button("Search", () => h.onLibrarySearch()));
  main.append(bar);
  if (state.loading && state.recipes.length === 0) {
    main.append(el("div", "empty", "Loading…"));
    return;
  }
  if (state.recipes.length === 0) {
    main.append(el("div", "empty", "No recipes match. Try Import."));
    return;
  }
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

function renderRecipe(main: HTMLElement, state: AppState, h: Handlers): void {
  const d = state.recipeDetail;
  if (!d) {
    main.append(el("div", "empty", "Loading recipe…"));
    return;
  }
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

  const actions = el("div", "toolbar");
  actions.append(
    button("← Back to library", () => h.onNav("library"), "ghost"),
    button("Delete recipe", () => h.onDeleteRecipe(), "danger"),
  );
  main.append(actions);
}

function renderPantry(main: HTMLElement, state: AppState, h: Handlers): void {
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
    return;
  }
  if (state.pantry.length === 0) {
    main.append(el("div", "empty", "Pantry is empty."));
    return;
  }
  const list = el("ul", "list");
  for (const p of state.pantry) {
    const li = document.createElement("li");
    const left = document.createElement("div");
    left.append(el("div", "title", p.name), el("div", "sub", p.kind));
    li.append(left);
    const right = el("div", "row-actions");
    right.append(el("span", "badge", `${formatQty(p.quantity_canonical)} ${p.unit_label}`));
    right.append(button("Remove", () => h.onPantryRemove(p.name, p.kind), "danger small"));
    li.append(right);
    list.append(li);
  }
  main.append(list);
}

function renderPlan(main: HTMLElement, state: AppState, h: Handlers): void {
  main.append(pageHeader("Plan", state.activePlan ? shortId(state.activePlan.id) : "new"));

  const form = el("div", "card form-grid");
  form.append(el("h3", "", "Generate meal plan"));
  form.append(
    labeledNumber("Days", state.planDays, (n) => h.onPlanDays(n)),
    labeledNumber("Meals / day", state.planMealsPerDay, (n) => h.onPlanMeals(n)),
  );
  form.append(
    labeledCheck("Time-of-day steering", state.planTod, (v) => h.onPlanTod(v)),
    labeledCheck("Save plan to database", state.planSave, (v) => h.onPlanSave(v)),
  );
  // TOML path + load/save
  const pathRow = el("div", "toolbar");
  const pathInput = document.createElement("input");
  pathInput.type = "text";
  pathInput.className = "input grow";
  pathInput.placeholder = "Path to nutrition_bounds.toml";
  pathInput.value = state.nutritionConfig;
  pathInput.addEventListener("input", () => h.onNutritionConfig(pathInput.value));
  pathRow.append(
    pathInput,
    button("Load TOML", () => h.onLoadNutritionConfig()),
    button("Save TOML", () => h.onSaveNutritionConfig()),
  );
  form.append(pathRow);

  // Full bounds editor (all scopes + category) — same as CLI/TOML
  const bf = ensurePlanBoundsForm(state.nutritionBounds);
  form.append(el("h3", "", "Nutrition bounds (full)"));
  form.append(
    el(
      "p",
      "muted",
      "Matches nutrition_bounds.toml: per_day / per_meal / plan min-max + ratio, and category whitelist/blacklist. Empty fields = unconstrained.",
    ),
  );
  form.append(bf.root);

  // CLI-style per-day overlays (applied on top of the form/TOML)
  form.append(el("h3", "", "Per-day CLI overlays (optional)"));
  form.append(
    el("p", "muted", "Same as CLI --min-kcal / --max-protein-g etc. Override the form for per_day only."),
  );
  const overlay = el("div", "bounds-grid");
  overlay.append(
    overlayField("min kcal", state.minKcal, h.onMinKcal),
    overlayField("max kcal", state.maxKcal, h.onMaxKcal),
    overlayField("min protein g", state.minProtein, h.onMinProtein),
    overlayField("max protein g", state.maxProtein, h.onMaxProtein),
    overlayField("min fat g", state.minFat, h.onMinFat),
    overlayField("max fat g", state.maxFat, h.onMaxFat),
    overlayField("min carbs g", state.minCarbs, h.onMinCarbs),
    overlayField("max carbs g", state.maxCarbs, h.onMaxCarbs),
  );
  form.append(overlay);

  form.append(el("h3", "", "Recipe pool (optional)"));
  form.append(
    el("p", "muted", "Same as CLI --pool: comma-separated recipe id prefixes. Empty = entire library."),
  );
  form.append(
    labeledText("Pool", state.pool, (v) => h.onPool(v), "id1,id2,…"),
  );

  const createBtn = button(state.busy ? "Planning… (UI stays responsive)" : "Create plan", () => h.onCreatePlan(), "primary");
  if (state.busy) createBtn.disabled = true;
  form.append(createBtn);
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
        li.classList.add("clickable");
        const left = document.createElement("div");
        const title = m.uses_pantry ? `${m.recipe_title} ★` : m.recipe_title;
        left.append(
          el("div", "title", title),
          el("div", "sub", `meal ${m.meal + 1} · ${shortId(m.recipe_id)}`),
        );
        li.append(left);
        if (m.uses_pantry) li.append(el("span", "badge", "pantry"));
        li.addEventListener("click", () => h.onOpenRecipe(m.recipe_id));
        ul.append(li);
      }
      card.append(ul);
    }
    const rat = el("pre", "rationale");
    rat.textContent = plan.rationale;
    card.append(el("h3", "", "Rationale"), rat);
    const actions = el("div", "toolbar");
    actions.append(
      button("Shopping list", () => h.onShop(), "primary"),
      button("Restock (buy + cook)", () => h.onRestock()),
    );
    card.append(actions);
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
      if (item.leftover > 0) li.append(el("span", "badge", `leftover ${formatQty(item.leftover)}`));
      ul.append(li);
    }
    shop.append(ul);
    main.append(shop);
  }
}

function renderImport(main: HTMLElement, state: AppState, h: Handlers): void {
  main.append(pageHeader("Import", "file · url · epub · auto"));
  const card = el("div", "card form-grid");
  card.append(el("h3", "", "Ingest a recipe source"));
  const sel = document.createElement("select");
  sel.className = "input";
  for (const s of ["auto", "file", "url", "epub"]) {
    const o = document.createElement("option");
    o.value = s;
    o.textContent = s;
    if (state.importSource === s) o.selected = true;
    sel.append(o);
  }
  sel.addEventListener("change", () => h.onImportSource(sel.value));
  const lab = el("label", "field");
  lab.append(el("span", "", "Source kind"), sel);
  card.append(lab);
  card.append(
    labeledText("Path or URL", state.importInput, (v) => h.onImportInput(v), "/path/to/recipe.json"),
  );
  card.append(button(state.busy ? "Importing…" : "Import", () => h.onImport(), "primary"));
  card.append(
    el(
      "p",
      "muted",
      "Uses the same ingest pipeline as the CLI. EPUB may save multiple recipes. Duplicates by source URL are skipped.",
    ),
  );
  main.append(card);
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
    if (page === "import") return { loading: false, error: null };
    return { loading: false };
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e), loading: false };
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
  input.addEventListener("change", () => onChange(Math.max(1, Number(input.value) || 1)));
  wrap.append(input);
  return wrap;
}

function overlayField(label: string, value: string, onChange: (v: string) => void): HTMLElement {
  const wrap = el("label", "field");
  wrap.append(el("span", "", label));
  const input = document.createElement("input");
  input.type = "number";
  input.step = "any";
  input.value = value;
  input.className = "input";
  input.addEventListener("input", () => onChange(input.value));
  wrap.append(input);
  return wrap;
}

function labeledText(
  label: string,
  value: string,
  onChange: (v: string) => void,
  placeholder = "",
): HTMLElement {
  const wrap = el("label", "field");
  wrap.append(el("span", "", label));
  const input = document.createElement("input");
  input.type = "text";
  input.value = value;
  input.placeholder = placeholder;
  input.className = "input";
  input.addEventListener("input", () => onChange(input.value));
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
