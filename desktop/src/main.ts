import "./styles.css";
import { createApi } from "./bridge";
import {
  ensurePlanBoundsForm,
  initialState,
  loadPageData,
  render,
  setPlanBoundsForm,
  type AppState,
  type Handlers,
  type Page,
} from "./app";

const root = document.querySelector<HTMLElement>("#app");
if (!root) throw new Error("#app missing");

const api = createApi();
window.__SR_API__ = api;

let state: AppState = initialState();

function paint(): void {
  render(root!, state, handlers);
}

/** Merge state; only re-render when paint=true (default). Use paint=false for keystroke fields. */
function set(patch: Partial<AppState>, opts: { paint?: boolean } = {}): void {
  state = { ...state, ...patch };
  if (opts.paint !== false) paint();
}

function numOrNull(s: string): number | null {
  const t = s.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

async function navigate(page: Page): Promise<void> {
  set({
    page,
    loading: true,
    error: null,
    notice: null,
    shop: page === "plan" ? state.shop : [],
    recipeDetail: page === "recipe" ? state.recipeDetail : null,
  });
  const patch = await loadPageData(api, page, state);
  set({ ...patch, page });
}

const handlers: Handlers = {
  onNav: (p) => {
    void navigate(p);
  },
  onOpenRecipe: (id) => {
    void (async () => {
      set({ page: "recipe", loading: true, error: null, recipeDetail: null, notice: null });
      try {
        const recipeDetail = await api.getRecipe(id);
        set({ recipeDetail, loading: false });
      } catch (e) {
        set({
          error: e instanceof Error ? e.message : String(e),
          loading: false,
          page: "library",
        });
      }
    })();
  },
  onDeleteRecipe: () => {
    void (async () => {
      if (!state.recipeDetail) return;
      if (!confirm(`Delete “${state.recipeDetail.title}”?`)) return;
      set({ busy: true, error: null, notice: null });
      try {
        await api.deleteRecipe(state.recipeDetail.id);
        const recipes = await api.listRecipes(state.libraryFilter || null);
        const status = await api.getStatus();
        // List/nav update is enough feedback — no success toast.
        set({
          recipes,
          status,
          recipeDetail: null,
          page: "library",
          busy: false,
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onPlanDays: (n) => set({ planDays: n }),
  onPlanMeals: (n) => set({ planMealsPerDay: n }),
  onPlanTod: (v) => set({ planTod: v }),
  onPlanSave: (v) => set({ planSave: v }),
  onNutritionConfig: (v) => set({ nutritionConfig: v }, { paint: false }),
  onLoadNutritionConfig: () => {
    void (async () => {
      const path = state.nutritionConfig.trim();
      if (!path) {
        set({ error: "Enter a path to a nutrition bounds file first." });
        return;
      }
      set({ busy: true, error: null, notice: null });
      try {
        const nutritionBounds = await api.loadNutritionConfig(path);
        setPlanBoundsForm(nutritionBounds);
        set({
          nutritionBounds,
          busy: false,
          notice: "Nutrition bounds loaded.",
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onSaveNutritionConfig: () => {
    void (async () => {
      const path = state.nutritionConfig.trim();
      if (!path) {
        set({ error: "Enter a path to save the nutrition bounds file." });
        return;
      }
      set({ busy: true, error: null, notice: null });
      try {
        const bounds = ensurePlanBoundsForm(state.nutritionBounds).read();
        await api.saveNutritionConfig(path, bounds);
        set({
          nutritionBounds: bounds,
          busy: false,
          notice: "Nutrition bounds saved.",
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onMinKcal: (v) => set({ minKcal: v }, { paint: false }),
  onMaxKcal: (v) => set({ maxKcal: v }, { paint: false }),
  onMinProtein: (v) => set({ minProtein: v }, { paint: false }),
  onMaxProtein: (v) => set({ maxProtein: v }, { paint: false }),
  onMinFat: (v) => set({ minFat: v }, { paint: false }),
  onMaxFat: (v) => set({ maxFat: v }, { paint: false }),
  onMinCarbs: (v) => set({ minCarbs: v }, { paint: false }),
  onMaxCarbs: (v) => set({ maxCarbs: v }, { paint: false }),
  onPool: (v) => set({ pool: v }, { paint: false }),
  onReadBounds: () => ensurePlanBoundsForm(state.nutritionBounds).read(),
  onCreatePlan: () => {
    void (async () => {
      const pathEl = root!.querySelector<HTMLInputElement>(
        'input[placeholder*="nutrition_bounds"]',
      );
      if (pathEl) state = { ...state, nutritionConfig: pathEl.value };
      const poolEl = root!.querySelector<HTMLInputElement>(
        'input[placeholder*="Leave empty"]',
      );
      if (poolEl) state = { ...state, pool: poolEl.value };

      // In-progress: button only ("Creating…"). No notice, no banner.
      set({ busy: true, error: null, notice: null });
      try {
        const bounds = ensurePlanBoundsForm(state.nutritionBounds).read();
        const poolTokens = state.pool
          .split(/[\n,]+/)
          .map((s) => s.trim())
          .filter(Boolean);
        const activePlan = await api.createPlan({
          days: state.planDays,
          meals_per_day: state.planMealsPerDay,
          time_of_day: state.planTod,
          save: state.planSave,
          bounds,
          nutrition_config: state.nutritionConfig.trim() || null,
          min_kcal: numOrNull(state.minKcal),
          max_kcal: numOrNull(state.maxKcal),
          min_protein_g: numOrNull(state.minProtein),
          max_protein_g: numOrNull(state.maxProtein),
          min_fat_g: numOrNull(state.minFat),
          max_fat_g: numOrNull(state.maxFat),
          min_carbs_g: numOrNull(state.minCarbs),
          max_carbs_g: numOrNull(state.maxCarbs),
          pool: poolTokens.length ? poolTokens : null,
        });
        const plans = state.planSave ? await api.listPlans() : state.plans;
        const status = await api.getStatus();
        // Plan card appears above options — no success toast needed.
        set({
          activePlan,
          plans,
          status,
          nutritionBounds: bounds,
          busy: false,
          notice: null,
          shop: [],
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onOpenPlan: (id) => {
    void (async () => {
      set({ busy: true, error: null, notice: null, shop: [] });
      try {
        const activePlan = await api.getPlan(id);
        set({ activePlan, busy: false });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onShop: () => {
    void (async () => {
      if (!state.activePlan) return;
      set({ busy: true, error: null, notice: null });
      try {
        const shop = await api.shopPlan(state.activePlan.id);
        set({
          shop,
          busy: false,
          // Empty list is not obvious — only message when there is nothing to show.
          notice: shop.length ? null : "Nothing to buy — pantry covers this plan.",
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onRestock: () => {
    void (async () => {
      if (!state.activePlan) return;
      if (!confirm("Update the pantry for this plan (purchases and cooked amounts)?")) {
        return;
      }
      set({ busy: true, error: null, notice: null });
      try {
        const result = await api.restockPlan(state.activePlan.id);
        const pantry = await api.listPantry();
        const status = await api.getStatus();
        // Restock result text carries counts users cannot see otherwise.
        set({ pantry, status, busy: false, notice: result.message });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onPantryLine: (v) => set({ pantryLine: v }, { paint: false }),
  onPantryAdd: () => {
    void (async () => {
      const input = root!.querySelector<HTMLInputElement>(
        'input[placeholder*="cups milk"]',
      );
      if (input) state = { ...state, pantryLine: input.value };
      const line = state.pantryLine.trim();
      if (!line) return;
      set({ busy: true, error: null, notice: null });
      try {
        const pantry = await api.pantryAdd(line);
        const status = await api.getStatus();
        set({
          pantry,
          status,
          pantryLine: "",
          busy: false,
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onPantryRemove: (name, kind) => {
    void (async () => {
      set({ busy: true, error: null, notice: null });
      try {
        const pantry = await api.pantryRemove(name, kind);
        const status = await api.getStatus();
        set({ pantry, status, busy: false });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onLibraryFilter: (v) => set({ libraryFilter: v }, { paint: false }),
  onLibrarySearch: () => {
    void (async () => {
      const input = root!.querySelector<HTMLInputElement>('input[type="search"]');
      if (input) state = { ...state, libraryFilter: input.value };
      set({ loading: true, error: null, notice: null });
      try {
        const recipes = await api.listRecipes(state.libraryFilter || null);
        set({ recipes, loading: false });
      } catch (e) {
        set({
          loading: false,
          error: e instanceof Error ? e.message : String(e),
        });
      }
    })();
  },
  onImportSource: (v) => set({ importSource: v }),
  onImportInput: (v) => set({ importInput: v }, { paint: false }),
  onImport: () => {
    void (async () => {
      const inputEl = root!.querySelector<HTMLInputElement>(
        'input[placeholder*="recipe"]',
      );
      if (inputEl) state = { ...state, importInput: inputEl.value };
      const input = state.importInput.trim();
      if (!input) {
        set({ error: "Path or URL required." });
        return;
      }
      set({ busy: true, error: null, notice: null });
      try {
        const result = await api.importSource(state.importSource, input);
        const status = await api.getStatus();
        const recipes = await api.listRecipes(null);
        // Import summary is not visible elsewhere (count + titles).
        set({
          busy: false,
          status,
          recipes,
          notice: result.message,
          importInput: "",
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
};

void navigate("home");
