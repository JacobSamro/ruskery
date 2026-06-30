import { test, expect } from "@playwright/test";

test.describe("unauthenticated", () => {
  test.use({ storageState: { cookies: [], origins: [] } });

  test("protected routes redirect to login", async ({ page }) => {
    await page.goto("/orgs/acme");
    await expect(page).toHaveURL(/\/login/);
    await page.goto("/settings");
    await expect(page).toHaveURL(/\/login/);
  });
});

test.describe("non-admin", () => {
  test.use({ storageState: "tests/e2e/.auth/dev.json" });

  test("cannot reach instance settings", async ({ page }) => {
    await page.goto("/settings");
    await expect(page).not.toHaveURL(/\/settings/);
  });

  test("cannot reach the organizations admin page", async ({ page }) => {
    await page.goto("/orgs");
    await expect(page).not.toHaveURL(/\/orgs$/);
  });
});
