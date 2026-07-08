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
    // Full CLI/TOML nutrition surface
    await expect(page.getByText("Nutrition bounds (full)")).toBeVisible();
    await expect(page.getByText("Per day")).toBeVisible();
    await expect(page.getByText("Per meal")).toBeVisible();
    await expect(page.getByText("Whole plan")).toBeVisible();
    await expect(page.getByText("Category filter")).toBeVisible();
    await expect(page.getByText("Per-day CLI overlays (optional)")).toBeVisible();
    await expect(page.getByRole("button", { name: "Load TOML" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Save TOML" })).toBeVisible();
    await page.getByRole("button", { name: "Create plan" }).click();
    await expect(page.getByText("Tomato Pasta ★")).toBeVisible();
    await page.getByRole("button", { name: "Shopping list" }).click();
    await expect(page.getByText("pasta", { exact: true })).toBeVisible();
    await expect(page).toHaveScreenshot("plan-shop.png", { fullPage: true });
  });

  test("import page", async ({ page }) => {
    await page.goto("/?mock=1");
    await page.getByRole("button", { name: "Import" }).click();
    await expect(page.getByRole("heading", { name: "Import" })).toBeVisible();
    await expect(page.getByText("Ingest a recipe source")).toBeVisible();
    await expect(page).toHaveScreenshot("import.png", { fullPage: true });
  });
});
