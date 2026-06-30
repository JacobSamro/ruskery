import { test, expect } from "@playwright/test";

// These specs log in with their own throwaway session instead of the shared
// admin storageState — signing out deletes the session server-side, which would
// otherwise invalidate the shared admin cookie for every later test.
test.describe("sign out", () => {
  test.use({ storageState: { cookies: [], origins: [] } });

  test.beforeEach(async ({ page }) => {
    await page.goto("/login");
    await page.getByTestId("login-username").fill("admin");
    await page.getByTestId("login-password").fill("supersecret");
    await page.getByRole("button", { name: /^sign in$/i }).click();
    await expect(page).not.toHaveURL(/\/login/);
  });

  test("asks for confirmation and cancels", async ({ page }) => {
    await page.getByTestId("sign-out").click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText(/sign out/i);
    await dialog.getByRole("button", { name: /cancel/i }).click();
    await expect(dialog).toBeHidden();
    await expect(page).not.toHaveURL(/\/login/);
  });

  test("confirming signs out", async ({ page }) => {
    await page.getByTestId("sign-out").click();
    await page
      .getByRole("alertdialog")
      .getByRole("button", { name: /^sign out$/i })
      .click();
    await expect(page).toHaveURL(/\/login/);
  });
});

test.describe("login", () => {
  test.use({ storageState: { cookies: [], origins: [] } });

  test("rejects bad credentials", async ({ page }) => {
    await page.goto("/login");
    await page.getByTestId("login-username").fill("admin");
    await page.getByTestId("login-password").fill("wrongpassword");
    await page.getByRole("button", { name: /^sign in$/i }).click();
    await expect(page.getByText(/invalid|unauthorized|incorrect|failed/i)).toBeVisible();
    await expect(page).toHaveURL(/\/login/);
  });

  test("accepts good credentials", async ({ page }) => {
    await page.goto("/login");
    await page.getByTestId("login-username").fill("admin");
    await page.getByTestId("login-password").fill("supersecret");
    await page.getByRole("button", { name: /^sign in$/i }).click();
    await expect(page).not.toHaveURL(/\/login/);
  });
});
