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

export type RecipeDetail = {
  id: string;
  title: string;
  category: string | null;
  servings: number | null;
  ingredients: string[];
  steps: string[];
  source: string;
};

export type PantryItemView = {
  name: string;
  kind: string;
  quantity_canonical: number;
  unit_label: string;
};

export type PlannedMealView = {
  day: number;
  meal: number;
  recipe_id: string;
  recipe_title: string;
  uses_pantry: boolean;
};

export type PlanView = {
  id: string;
  days: number;
  meals_per_day: number;
  meals: PlannedMealView[];
  rationale: string;
};

export type PlanSummary = {
  id: string;
  days: number;
  meals_per_day: number;
  meal_count: number;
};

export type CreatePlanArgs = {
  days: number;
  meals_per_day: number;
  time_of_day: boolean;
  save: boolean;
};

export type ShopItemView = {
  name: string;
  need: number;
  unit: string;
  leftover: number;
};

export type Api = {
  getStatus: () => Promise<DbStatus>;
  listRecipes: (filter?: string | null) => Promise<RecipeSummary[]>;
  getRecipe: (id: string) => Promise<RecipeDetail>;
  listPantry: () => Promise<PantryItemView[]>;
  pantryAdd: (line: string) => Promise<PantryItemView[]>;
  pantryRemove: (name: string, kind?: string | null) => Promise<PantryItemView[]>;
  listPlans: () => Promise<PlanSummary[]>;
  getPlan: (id: string) => Promise<PlanView>;
  createPlan: (args: CreatePlanArgs) => Promise<PlanView>;
  shopPlan: (id: string) => Promise<ShopItemView[]>;
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

const MOCK_DETAILS: Record<string, RecipeDetail> = {
  "11111111-aaaa-bbbb-cccc-000000000001": {
    id: "11111111-aaaa-bbbb-cccc-000000000001",
    title: "Tomato Pasta",
    category: "Dinner",
    servings: 4,
    ingredients: ["400 g pasta", "2 cups tomato sauce", "1 tbsp olive oil", "2 cloves garlic", "salt"],
    steps: ["Boil pasta.", "Warm sauce with garlic and oil.", "Combine and serve."],
    source: "mock",
  },
  "22222222-aaaa-bbbb-cccc-000000000002": {
    id: "22222222-aaaa-bbbb-cccc-000000000002",
    title: "Watermelon Cooler",
    category: "Beverage",
    servings: 2,
    ingredients: ["2 cups watermelon", "1 cup lemonade", "ice"],
    steps: ["Blend watermelon with lemonade.", "Serve over ice."],
    source: "mock",
  },
  "33333333-aaaa-bbbb-cccc-000000000003": {
    id: "33333333-aaaa-bbbb-cccc-000000000003",
    title: "Tahini Sauce",
    category: "Sauce",
    servings: 6,
    ingredients: ["1/2 cup tahini", "2 tbsp lemon juice", "1 garlic clove", "water"],
    steps: ["Whisk tahini with lemon and garlic.", "Thin with water."],
    source: "mock",
  },
};

let mockPantry: PantryItemView[] = [
  { name: "flour", kind: "mass", quantity_canonical: 500, unit_label: "g" },
  { name: "milk", kind: "volume", quantity_canonical: 750, unit_label: "ml" },
];

const MOCK_PLAN: PlanView = {
  id: "plan-mock-0001-aaaa-bbbb-cccc-dddd",
  days: 2,
  meals_per_day: 1,
  meals: [
    {
      day: 0,
      meal: 0,
      recipe_id: "11111111-aaaa-bbbb-cccc-000000000001",
      recipe_title: "Tomato Pasta",
      uses_pantry: true,
    },
    {
      day: 1,
      meal: 0,
      recipe_id: "22222222-aaaa-bbbb-cccc-000000000002",
      recipe_title: "Watermelon Cooler",
      uses_pantry: false,
    },
  ],
  rationale:
    "Min-union planner: 2 meal(s) over 2 day(s), no recipe repeats.\n  Pool: 3 unique recipe(s)\n  8 distinct ingredient key(s)\n  Pantry: 1 of 2 on-hand item(s) used; 6 key(s) not covered by pantry stock",
};

const MOCK_PLANS: PlanSummary[] = [
  { id: MOCK_PLAN.id, days: 2, meals_per_day: 1, meal_count: 2 },
];

const MOCK_SHOP: ShopItemView[] = [
  { name: "pasta", need: 400, unit: "g", leftover: 0 },
  { name: "tomato sauce", need: 480, unit: "ml", leftover: 20 },
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

function mockApi(): Api {
  return {
    getStatus: async () => ({
      ...MOCK_STATUS,
      pantry_count: mockPantry.length,
      plan_count: MOCK_PLANS.length,
    }),
    listRecipes: async (filter) => {
      const f = (filter ?? "").trim().toLowerCase();
      if (!f) return structuredClone(MOCK_RECIPES);
      return MOCK_RECIPES.filter((r) => r.title.toLowerCase().includes(f));
    },
    getRecipe: async (id) => {
      const hit =
        MOCK_DETAILS[id] ??
        Object.values(MOCK_DETAILS).find((r) => r.id.startsWith(id));
      if (!hit) throw new Error(`no recipe matching '${id}'`);
      return structuredClone(hit);
    },
    listPantry: async () => structuredClone(mockPantry),
    pantryAdd: async (line) => {
      // Minimal mock: append a fake row from free text.
      const name = line.replace(/^\d+(\.\d+)?\s*\w+\s+/, "").trim() || "item";
      mockPantry = [
        ...mockPantry.filter((p) => p.name !== name),
        { name, kind: "count", quantity_canonical: 1, unit_label: "ea" },
      ];
      return structuredClone(mockPantry);
    },
    pantryRemove: async (name) => {
      mockPantry = mockPantry.filter((p) => p.name !== name);
      return structuredClone(mockPantry);
    },
    listPlans: async () => structuredClone(MOCK_PLANS),
    getPlan: async (id) => {
      if (!MOCK_PLAN.id.startsWith(id) && id !== MOCK_PLAN.id) {
        throw new Error(`no plan matching '${id}'`);
      }
      return structuredClone(MOCK_PLAN);
    },
    createPlan: async (args) => {
      const plan = structuredClone(MOCK_PLAN);
      plan.days = args.days;
      plan.meals_per_day = args.meals_per_day;
      return plan;
    },
    shopPlan: async () => structuredClone(MOCK_SHOP),
  };
}

export function createApi(): Api {
  if (useMock()) return mockApi();
  return {
    getStatus: () => tauriInvoke<DbStatus>("get_status"),
    listRecipes: (filter) =>
      tauriInvoke<RecipeSummary[]>("list_recipes", { filter: filter ?? null }),
    getRecipe: (id) => tauriInvoke<RecipeDetail>("get_recipe", { id }),
    listPantry: () => tauriInvoke<PantryItemView[]>("list_pantry"),
    pantryAdd: (line) => tauriInvoke<PantryItemView[]>("pantry_add", { line }),
    pantryRemove: (name, kind) =>
      tauriInvoke<PantryItemView[]>("pantry_remove", { name, kind: kind ?? null }),
    listPlans: () => tauriInvoke<PlanSummary[]>("list_plans"),
    getPlan: (id) => tauriInvoke<PlanView>("get_plan", { id }),
    createPlan: (args) => tauriInvoke<PlanView>("create_plan", { args }),
    shopPlan: (id) => tauriInvoke<ShopItemView[]>("shop_plan", { id }),
  };
}
