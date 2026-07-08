/** Nutrition bounds form helpers — mirrors nutrition_bounds.toml + CLI overlays. */

import type { MacroBounds, MacroRange, MacroRatio, NutritionBounds } from "./bridge";

export function emptyBounds(): NutritionBounds {
  return {
    per_day: emptyMacro(),
    per_meal: emptyMacro(),
    plan: emptyMacro(),
    category: { whitelist: [], blacklist: [] },
  };
}

function emptyMacro(): MacroBounds {
  return {
    kcal: {},
    protein_g: {},
    fat_g: {},
    carbs_g: {},
    ratio: {},
  };
}

function numOrNull(s: string): number | null {
  const t = s.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

function rangeFromInputs(minEl: HTMLInputElement, maxEl: HTMLInputElement): MacroRange {
  const min = numOrNull(minEl.value);
  const max = numOrNull(maxEl.value);
  const out: MacroRange = {};
  if (min != null) out.min = min;
  if (max != null) out.max = max;
  return out;
}

function setRange(minEl: HTMLInputElement, maxEl: HTMLInputElement, r?: MacroRange | null): void {
  minEl.value = r?.min != null ? String(r.min) : "";
  maxEl.value = r?.max != null ? String(r.max) : "";
}

/** Build a nutrition scope editor (per_day / per_meal / plan). */
export function scopeEditor(
  title: string,
  scope: MacroBounds | undefined,
  onChange: () => void,
): { root: HTMLElement; read: () => MacroBounds } {
  const root = document.createElement("details");
  root.className = "bounds-scope";
  root.open = true;
  const sum = document.createElement("summary");
  sum.textContent = title;
  root.append(sum);

  const grid = document.createElement("div");
  grid.className = "bounds-grid";

  const fields: {
    key: keyof MacroBounds;
    label: string;
    min: HTMLInputElement;
    max: HTMLInputElement;
  }[] = [];

  for (const [key, label] of [
    ["kcal", "Calories (kcal)"],
    ["protein_g", "Protein (g)"],
    ["fat_g", "Fat (g)"],
    ["carbs_g", "Carbs (g)"],
  ] as const) {
    const row = document.createElement("div");
    row.className = "bounds-row";
    row.append(labelEl(label));
    const min = numInput("min");
    const max = numInput("max");
    const r = scope?.[key] as MacroRange | undefined;
    setRange(min, max, r);
    min.addEventListener("input", onChange);
    max.addEventListener("input", onChange);
    row.append(min, max);
    grid.append(row);
    fields.push({ key, label, min, max });
  }

  // Ratio
  const ratio = scope?.ratio ?? {};
  const ratioBox = document.createElement("div");
  ratioBox.className = "bounds-ratio";
  ratioBox.append(labelEl("Macro ratio targets (% of P+F+C grams)"));
  const ratioRow = document.createElement("div");
  ratioRow.className = "bounds-row ratio";
  const p = numInput("P%");
  const f = numInput("F%");
  const c = numInput("C%");
  const tol = numInput("±tol");
  p.value = ratio.protein != null ? String(ratio.protein) : "";
  f.value = ratio.fat != null ? String(ratio.fat) : "";
  c.value = ratio.carb != null ? String(ratio.carb) : "";
  tol.value = ratio.tolerance != null ? String(ratio.tolerance) : "";
  for (const el of [p, f, c, tol]) {
    el.addEventListener("input", onChange);
    ratioRow.append(el);
  }
  ratioBox.append(ratioRow);
  grid.append(ratioBox);
  root.append(grid);

  return {
    root,
    read: () => {
      const out: MacroBounds = {};
      for (const f of fields) {
        const r = rangeFromInputs(f.min, f.max);
        if (r.min != null || r.max != null) {
          (out as Record<string, MacroRange>)[f.key] = r;
        }
      }
      const ratioOut: MacroRatio = {};
      const pv = numOrNull(p.value);
      const fv = numOrNull(f.value);
      const cv = numOrNull(c.value);
      const tv = numOrNull(tol.value);
      if (pv != null) ratioOut.protein = pv;
      if (fv != null) ratioOut.fat = fv;
      if (cv != null) ratioOut.carb = cv;
      if (tv != null) ratioOut.tolerance = tv;
      if (Object.keys(ratioOut).length) out.ratio = ratioOut;
      return out;
    },
  };
}

export type BoundsForm = {
  root: HTMLElement;
  read: () => NutritionBounds;
  set: (b: NutritionBounds) => void;
};

export function boundsForm(
  initial: NutritionBounds,
  onChange: () => void,
): BoundsForm {
  const root = document.createElement("div");
  root.className = "bounds-form";

  let day = scopeEditor("Per day", initial.per_day, onChange);
  let meal = scopeEditor("Per meal", initial.per_meal, onChange);
  let plan = scopeEditor("Whole plan", initial.plan, onChange);

  const cat = document.createElement("details");
  cat.className = "bounds-scope";
  cat.open = true;
  const catSum = document.createElement("summary");
  catSum.textContent = "Category filter";
  cat.append(catSum);
  const white = document.createElement("textarea");
  white.className = "input textarea";
  white.placeholder = "Whitelist (one per line or comma-separated). Empty = no whitelist.";
  white.value = (initial.category?.whitelist ?? []).join("\n");
  white.addEventListener("input", onChange);
  const black = document.createElement("textarea");
  black.className = "input textarea";
  black.placeholder = "Blacklist (one per line or comma-separated)";
  black.value = (initial.category?.blacklist ?? []).join("\n");
  black.addEventListener("input", onChange);
  cat.append(labelEl("Whitelist"), white, labelEl("Blacklist"), black);

  function rebuild(b: NutritionBounds): void {
    root.innerHTML = "";
    day = scopeEditor("Per day", b.per_day, onChange);
    meal = scopeEditor("Per meal", b.per_meal, onChange);
    plan = scopeEditor("Whole plan", b.plan, onChange);
    white.value = (b.category?.whitelist ?? []).join("\n");
    black.value = (b.category?.blacklist ?? []).join("\n");
    root.append(day.root, meal.root, plan.root, cat);
  }

  rebuild(initial);

  return {
    root,
    read: () => ({
      per_day: day.read(),
      per_meal: meal.read(),
      plan: plan.read(),
      category: {
        whitelist: splitTokens(white.value),
        blacklist: splitTokens(black.value),
      },
    }),
    set: (b) => rebuild(b),
  };
}

function splitTokens(text: string): string[] {
  return text
    .split(/[\n,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function numInput(placeholder: string): HTMLInputElement {
  const i = document.createElement("input");
  i.type = "number";
  i.step = "any";
  i.placeholder = placeholder;
  i.className = "input";
  return i;
}

function labelEl(text: string): HTMLElement {
  const s = document.createElement("div");
  s.className = "bounds-label";
  s.textContent = text;
  return s;
}
