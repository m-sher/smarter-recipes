/**
 * Data bridge: Tauri invoke in the app, deterministic mock fixtures for visual tests.
 * Set `window.__SR_MOCK__ = true` (or `?mock=1`) before boot to force mock mode.
 */

export type DbStatus = {
  path: string;
  recipe_count: number;
  plan_count: number;
  pantry_count: number;
};

export type RecipeSummary = {
  id: string;
  title: string;
  category: string | null;
  ingredient_count: number;
};

export type PantryItemView = {
  name: string;
  kind: string;
  quantity_canonical: number;
  unit_label: string;
};

export type Api = {
  getStatus: () => Promise<DbStatus>;
  listRecipes: (filter?: string | null) => Promise<RecipeSummary[]>;
  listPantry: () => Promise<PantryItemView[]>;
};

declare global {
  interface Window {
    __SR_MOCK__?: boolean;
    __SR_API__?: Api;
  }
}

const MOCK_STATUS: DbStatus = {
  path: "/mock/recipes.db",
  recipe_count: 3,
  plan_count: 1,
  pantry_count: 2,
};

const MOCK_RECIPES: RecipeSummary[] = [
  {
    id: "11111111-aaaa-bbbb-cccc-000000000001",
    title: "Tomato Pasta",
    category: "Dinner",
    ingredient_count: 5,
  },
  {
    id: "22222222-aaaa-bbbb-cccc-000000000002",
    title: "Watermelon Cooler",
    category: "Beverage",
    ingredient_count: 3,
  },
  {
    id: "33333333-aaaa-bbbb-cccc-000000000003",
    title: "Tahini Sauce",
    category: "Sauce",
    ingredient_count: 4,
  },
];

const MOCK_PANTRY: PantryItemView[] = [
  { name: "flour", kind: "mass", quantity_canonical: 500, unit_label: "g" },
  { name: "milk", kind: "volume", quantity_canonical: 750, unit_label: "ml" },
];

function useMock(): boolean {
  if (typeof window === "undefined") return true;
  if (window.__SR_MOCK__ === true) return true;
  const q = new URLSearchParams(window.location.search);
  return q.get("mock") === "1";
}

async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function createApi(): Api {
  if (useMock()) {
    return {
      getStatus: async () => structuredClone(MOCK_STATUS),
      listRecipes: async (filter) => {
        const f = (filter ?? "").trim().toLowerCase();
        if (!f) return structuredClone(MOCK_RECIPES);
        return MOCK_RECIPES.filter((r) => r.title.toLowerCase().includes(f));
      },
      listPantry: async () => structuredClone(MOCK_PANTRY),
    };
  }
  return {
    getStatus: () => tauriInvoke<DbStatus>("get_status"),
    listRecipes: (filter) =>
      tauriInvoke<RecipeSummary[]>("list_recipes", { filter: filter ?? null }),
    listPantry: () => tauriInvoke<PantryItemView[]>("list_pantry"),
  };
}
