import { test, expect } from "@playwright/test";

/**
 * Capture design frames of the mock-backed UI and compare to golden snapshots.
 * Update baselines intentionally: `npm run test:visual:update`
 */
test.describe("shell frames (mock data)", () => {
  test.beforeEach(async ({ page }) => {
    // Force mock bridge before any app code runs.
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
    await expect(page.getByText("Watermelon Cooler")).toBeVisible();
    await expect(page).toHaveScreenshot("library.png", { fullPage: true });
  });

  test("pantry list", async ({ page }) => {
    await page.goto("/?mock=1");
    await page.getByRole("button", { name: "Pantry" }).click();
    await expect(page.getByRole("heading", { name: "Pantry" })).toBeVisible();
    await expect(page.getByText("flour")).toBeVisible();
    await expect(page.getByText("500 g")).toBeVisible();
    await expect(page).toHaveScreenshot("pantry.png", { fullPage: true });
  });
});
