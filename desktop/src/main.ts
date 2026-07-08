import "./styles.css";
import { createApi } from "./bridge";
import {
  initialState,
  loadPageData,
  render,
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

function set(patch: Partial<AppState>): void {
  state = { ...state, ...patch };
  paint();
}

async function navigate(page: Page): Promise<void> {
  set({ page, loading: true, error: null, notice: null, shop: page === "plan" ? state.shop : [] });
  const patch = await loadPageData(api, page, state);
  set({ ...patch, page });
}

const handlers: Handlers = {
  onNav: (p) => {
    void navigate(p);
  },
  onOpenRecipe: (id) => {
    void (async () => {
      set({ page: "recipe", loading: true, error: null, recipeDetail: null });
      try {
        const recipeDetail = await api.getRecipe(id);
        set({ recipeDetail, loading: false });
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e), loading: false, page: "library" });
      }
    })();
  },
  onPlanDays: (n) => set({ planDays: n }),
  onPlanMeals: (n) => set({ planMealsPerDay: n }),
  onPlanTod: (v) => set({ planTod: v }),
  onPlanSave: (v) => set({ planSave: v }),
  onCreatePlan: () => {
    void (async () => {
      set({ busy: true, error: null, notice: null, shop: [] });
      try {
        const activePlan = await api.createPlan({
          days: state.planDays,
          meals_per_day: state.planMealsPerDay,
          time_of_day: state.planTod,
          save: state.planSave,
        });
        const plans = state.planSave ? await api.listPlans() : state.plans;
        const status = await api.getStatus();
        set({
          activePlan,
          plans,
          status,
          busy: false,
          notice: state.planSave ? "Plan saved." : "Plan generated (not saved).",
        });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onOpenPlan: (id) => {
    void (async () => {
      set({ busy: true, error: null, shop: [] });
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
      set({ busy: true, error: null });
      try {
        const shop = await api.shopPlan(state.activePlan.id);
        set({ shop, busy: false, notice: shop.length ? null : "Nothing to buy (fully covered)." });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onPantryLine: (v) => set({ pantryLine: v }),
  onPantryAdd: () => {
    void (async () => {
      const line = state.pantryLine.trim();
      if (!line) return;
      set({ busy: true, error: null, notice: null });
      try {
        const pantry = await api.pantryAdd(line);
        const status = await api.getStatus();
        set({ pantry, status, pantryLine: "", busy: false, notice: "Added to pantry." });
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
        set({ pantry, status, busy: false, notice: `Removed ${name}.` });
      } catch (e) {
        set({ busy: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
  onLibraryFilter: (v) => set({ libraryFilter: v }),
  onLibrarySearch: () => {
    void (async () => {
      set({ loading: true, error: null });
      try {
        const recipes = await api.listRecipes(state.libraryFilter || null);
        set({ recipes, loading: false });
      } catch (e) {
        set({ loading: false, error: e instanceof Error ? e.message : String(e) });
      }
    })();
  },
};

void navigate("home");
