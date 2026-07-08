/**
 * Data bridge: Tauri invoke in the app, deterministic mock fixtures for visual tests.
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


export type MacroRange = { min?: number | null; max?: number | null };
export type MacroRatio = {
  protein?: number | null;
  fat?: number | null;
  carb?: number | null;
  tolerance?: number | null;
};
export type MacroBounds = {
  kcal?: MacroRange;
  protein_g?: MacroRange;
  fat_g?: MacroRange;
  carbs_g?: MacroRange;
  ratio?: MacroRatio;
};
export type CategoryFilter = {
  whitelist?: string[];
  blacklist?: string[];
};
export type NutritionBounds = {
  per_day?: MacroBounds;
  per_meal?: MacroBounds;
  plan?: MacroBounds;
  category?: CategoryFilter;
};

export type CreatePlanArgs = {
  days: number;
  meals_per_day: number;
  time_of_day: boolean;
  save: boolean;
  bounds?: NutritionBounds | null;
  nutrition_config?: string | null;
  min_kcal?: number | null;
  max_kcal?: number | null;
  min_protein_g?: number | null;
  max_protein_g?: number | null;
  min_fat_g?: number | null;
  max_fat_g?: number | null;
  min_carbs_g?: number | null;
  max_carbs_g?: number | null;
  pool?: string[] | null;
};

export type ShopItemView = {
  name: string;
  need: number;
  unit: string;
  leftover: number;
};

export type RestockResult = {
  additions: number;
  deductions: number;
  message: string;
};

export type ImportResult = {
  saved: number;
  titles: string[];
  message: string;
};

export type Api = {
  getStatus: () => Promise<DbStatus>;
  listRecipes: (filter?: string | null) => Promise<RecipeSummary[]>;
  getRecipe: (id: string) => Promise<RecipeDetail>;
  deleteRecipe: (id: string) => Promise<void>;
  listPantry: () => Promise<PantryItemView[]>;
  pantryAdd: (line: string) => Promise<PantryItemView[]>;
  pantryRemove: (name: string, kind?: string | null) => Promise<PantryItemView[]>;
  listPlans: () => Promise<PlanSummary[]>;
  getPlan: (id: string) => Promise<PlanView>;
  createPlan: (args: CreatePlanArgs) => Promise<PlanView>;
  loadNutritionConfig: (path: string) => Promise<NutritionBounds>;
  parseNutritionToml: (text: string) => Promise<NutritionBounds>;
  defaultNutritionBounds: () => Promise<NutritionBounds>;
  nutritionBoundsToToml: (bounds: NutritionBounds) => Promise<string>;
  saveNutritionConfig: (path: string, bounds: NutritionBounds) => Promise<void>;
  shopPlan: (id: string) => Promise<ShopItemView[]>;
  restockPlan: (id: string) => Promise<RestockResult>;
  importSource: (source: string, input: string) => Promise<ImportResult>;
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
  { id: "11111111-aaaa-bbbb-cccc-000000000001", title: "Tomato Pasta", category: "Dinner", ingredient_count: 5 },
  { id: "22222222-aaaa-bbbb-cccc-000000000002", title: "Watermelon Cooler", category: "Beverage", ingredient_count: 3 },
  { id: "33333333-aaaa-bbbb-cccc-000000000003", title: "Tahini Sauce", category: "Sauce", ingredient_count: 4 },
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
    { day: 0, meal: 0, recipe_id: "11111111-aaaa-bbbb-cccc-000000000001", recipe_title: "Tomato Pasta", uses_pantry: true },
    { day: 1, meal: 0, recipe_id: "22222222-aaaa-bbbb-cccc-000000000002", recipe_title: "Watermelon Cooler", uses_pantry: false },
  ],
  rationale:
    "Min-union planner: 2 meal(s) over 2 day(s), no recipe repeats.\n  Pool: 3 unique recipe(s)\n  8 distinct ingredient key(s)\n  Pantry: 1 of 2 on-hand item(s) used; 6 key(s) not covered by pantry stock",
};

let mockPlans: PlanSummary[] = [
  { id: MOCK_PLAN.id, days: 2, meals_per_day: 1, meal_count: 2 },
];

const MOCK_SHOP: ShopItemView[] = [
  { name: "pasta", need: 400, unit: "g", leftover: 0 },
  { name: "tomato sauce", need: 480, unit: "ml", leftover: 20 },
];

function useMock(): boolean {
  if (typeof window === "undefined") return true;
  if (window.__SR_MOCK__ === true) return true;
  return new URLSearchParams(window.location.search).get("mock") === "1";
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
      plan_count: mockPlans.length,
      recipe_count: MOCK_RECIPES.length,
    }),
    listRecipes: async (filter) => {
      const f = (filter ?? "").trim().toLowerCase();
      if (!f) return structuredClone(MOCK_RECIPES);
      return MOCK_RECIPES.filter((r) => r.title.toLowerCase().includes(f));
    },
    getRecipe: async (id) => {
      const hit = MOCK_DETAILS[id] ?? Object.values(MOCK_DETAILS).find((r) => r.id.startsWith(id));
      if (!hit) throw new Error(`no recipe matching '${id}'`);
      return structuredClone(hit);
    },
    deleteRecipe: async (id) => {
      if (!MOCK_DETAILS[id] && !Object.values(MOCK_DETAILS).some((r) => r.id.startsWith(id))) {
        throw new Error(`no recipe matching '${id}'`);
      }
    },
    listPantry: async () => structuredClone(mockPantry),
    pantryAdd: async (line) => {
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
    listPlans: async () => structuredClone(mockPlans),
    getPlan: async (id) => {
      if (!MOCK_PLAN.id.startsWith(id) && id !== MOCK_PLAN.id) throw new Error(`no plan matching '${id}'`);
      return structuredClone(MOCK_PLAN);
    },
    createPlan: async (args) => {
      const plan = structuredClone(MOCK_PLAN);
      plan.days = args.days;
      plan.meals_per_day = args.meals_per_day;
      if (args.nutrition_config) {
        plan.rationale += "\n  Nutrition constraints satisfied";
      }
      if (args.save && !mockPlans.some((p) => p.id === plan.id)) {
        mockPlans = [{ id: plan.id, days: plan.days, meals_per_day: plan.meals_per_day, meal_count: plan.meals.length }, ...mockPlans];
      }
      return plan;
    },
    loadNutritionConfig: async () => ({
      per_day: {
        kcal: { min: 800, max: 1500 },
        protein_g: { min: 80 },
        ratio: { protein: 40, fat: 30, carb: 30, tolerance: 5 },
      },
      per_meal: { kcal: { min: 100 } },
      plan: {},
      category: { blacklist: ["Sauce", "Dressing", "Dessert"], whitelist: [] },
    }),
    parseNutritionToml: async () => ({ per_day: {}, per_meal: {}, plan: {}, category: { whitelist: [], blacklist: [] } }),
    defaultNutritionBounds: async () => ({ per_day: {}, per_meal: {}, plan: {}, category: { whitelist: [], blacklist: [] } }),
    nutritionBoundsToToml: async () => "# empty bounds\n",
    saveNutritionConfig: async () => {},
    shopPlan: async () => structuredClone(MOCK_SHOP),
    restockPlan: async () => ({
      additions: 2,
      deductions: 3,
      message: "Restocked: 2 purchase line(s), 3 cooked deduction(s). Leftovers remain in pantry.",
    }),
    importSource: async (_source, input) => ({
      saved: 1,
      titles: [`Imported ${input.split("/").pop() || input}`],
      message: "Saved 1 recipe(s)",
    }),
  };
}

export function createApi(): Api {
  if (useMock()) return mockApi();
  return {
    getStatus: () => tauriInvoke("get_status"),
    listRecipes: (filter) => tauriInvoke("list_recipes", { filter: filter ?? null }),
    getRecipe: (id) => tauriInvoke("get_recipe", { id }),
    deleteRecipe: (id) => tauriInvoke("delete_recipe", { id }),
    listPantry: () => tauriInvoke("list_pantry"),
    pantryAdd: (line) => tauriInvoke("pantry_add", { line }),
    pantryRemove: (name, kind) => tauriInvoke("pantry_remove", { name, kind: kind ?? null }),
    listPlans: () => tauriInvoke("list_plans"),
    getPlan: (id) => tauriInvoke("get_plan", { id }),
    createPlan: (args) => tauriInvoke("create_plan", { args }),
    loadNutritionConfig: (path) => tauriInvoke("load_nutrition_config", { path }),
    parseNutritionToml: (text) => tauriInvoke("parse_nutrition_toml", { text }),
    defaultNutritionBounds: () => tauriInvoke("default_nutrition_bounds"),
    nutritionBoundsToToml: (bounds) => tauriInvoke("nutrition_bounds_to_toml", { bounds }),
    saveNutritionConfig: (path, bounds) =>
      tauriInvoke("save_nutrition_config", { path, bounds }),
    shopPlan: (id) => tauriInvoke("shop_plan", { id }),
    restockPlan: (id) => tauriInvoke("restock_plan", { id }),
    importSource: (source, input) => tauriInvoke("import_source", { source, input }),
  };
}
