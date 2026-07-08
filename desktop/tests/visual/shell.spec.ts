import { test, expect } from "@playwright/test";

test.describe("shell frames (mock data)", () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      (window as unknown as { __SR_MOCK__: boolean }).__SR_MOCK__ = true;
    });
  });

  test("home dashboard", async ({ page }) => {
    await page.goto("/?mock=1");
    await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
    await expect(page.locator(".stat .label", { hasText: "Recipes" })).toBeVisible();
    await expect(page.locator(".stat .value").first()).toHaveText("3");
    await expect(page).toHaveScreenshot("home.png", { fullPage: true });
  });

  test("library list", async ({ page }) => {
    await page.goto("/?mock=1");
    await page.getByRole("button", { name: "Library" }).click();
    await expect(page.getByRole("heading", { name: "Library" })).toBeVisible();
    await expect(page.getByText("Tomato Pasta")).toBeVisible();
    await expect(page).toHaveScreenshot("library.png", { fullPage: true });
  });

  test("recipe detail", async ({ page }) => {
    await page.goto("/?mock=1");
    await page.getByRole("button", { name: "Library" }).click();
    await page.getByText("Tomato Pasta").click();
    await expect(page.getByRole("heading", { name: "Tomato Pasta" })).toBeVisible();
    await expect(page.getByText("400 g pasta")).toBeVisible();
    await expect(page).toHaveScreenshot("recipe.png", { fullPage: true });
  });

  test("pantry list", async ({ page }) => {
    await page.goto("/?mock=1");
    await page.getByRole("button", { name: "Pantry" }).click();
    await expect(page.getByRole("heading", { name: "Pantry" })).toBeVisible();
    await expect(page.getByText("flour")).toBeVisible();
    await expect(page).toHaveScreenshot("pantry.png", { fullPage: true });
  });

  test("plan with schedule and shop", async ({ page }) => {
    await page.goto("/?mock=1");
    await page.getByRole("button", { name: "Plan" }).click();
    await expect(page.getByRole("heading", { name: "Plan", exact: true })).toBeVisible();
    // Outer sections default closed; nested scopes stay closed until expanded.
    await expect(page.getByText("Nutrition bounds")).toBeVisible();
    await expect(page.getByText("Per-day overrides")).toBeVisible();
    await expect(page.getByText("Recipe pool")).toBeVisible();
    // Bodies exist in the DOM but must not be visible while collapsed.
    await expect(page.getByText("Calories (kcal)").first()).toBeHidden();
    await page.getByText("Nutrition bounds").click();
    await expect(page.getByText("Per day")).toBeVisible();
    await expect(page.getByText("Per meal")).toBeVisible();
    await expect(page.getByText("Whole plan")).toBeVisible();
    await expect(page.getByText("Category filter")).toBeVisible();
    await expect(page.getByText("Calories (kcal)").first()).toBeHidden();
    await page.getByText("Per day").click();
    await expect(page.getByText("Calories (kcal)").first()).toBeVisible();
    await page.getByText("Per day").click(); // re-collapse
    await expect(page.getByText("Calories (kcal)").first()).toBeHidden();
    await expect(page.getByRole("button", { name: "Load" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Save", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Create plan" }).click();
    // Result appears above the options form.
    await expect(page.getByText("Tomato Pasta ★")).toBeVisible();
    const planHeading = page.getByRole("heading", { name: /^Plan / });
    const optionsHeading = page.getByRole("heading", { name: "Generate meal plan" });
    await expect(planHeading).toBeVisible();
    const planBox = await planHeading.boundingBox();
    const optionsBox = await optionsHeading.boundingBox();
    expect(planBox && optionsBox && planBox.y < optionsBox.y).toBeTruthy();
    await page.getByRole("button", { name: "Shopping list" }).click();
    await expect(page.getByText("pasta", { exact: true })).toBeVisible();
    await expect(page).toHaveScreenshot("plan-shop.png", { fullPage: true });
    // Results are dismissable (plan dismiss also clears shop).
    await page.getByRole("button", { name: "Dismiss" }).first().click();
    await expect(page.getByText("Tomato Pasta ★")).toHaveCount(0);
    await expect(page.getByText("pasta", { exact: true })).toHaveCount(0);
  });

  test("import page", async ({ page }) => {
    await page.goto("/?mock=1");
    await page.getByRole("button", { name: "Import" }).click();
    await expect(page.getByRole("heading", { name: "Import", exact: true })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Import recipes" })).toBeVisible();
    await expect(page).toHaveScreenshot("import.png", { fullPage: true });
  });
});
