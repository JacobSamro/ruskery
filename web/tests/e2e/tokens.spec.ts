import { test, expect } from "@playwright/test";
import { uniq, chooseOption } from "./fixtures/helpers";

test.describe("access tokens (admin)", () => {
  test("create, reveal, and revoke a token", async ({ page }) => {
    const name = uniq("ci-");
    await page.goto("/tokens");
    await page.getByRole("button", { name: /new token/i }).click();

    await page.getByPlaceholder("laptop").fill(name);
    await chooseOption(page, "token-scope", "All my access");
    await chooseOption(page, "token-perm", /Full/);
    await page.getByRole("button", { name: /^create$/i }).click();

    // Secret is shown exactly once (scope to the reveal dialog, not the table).
    await expect(page.getByText(/copy it now/i)).toBeVisible();
    await page.getByRole("button", { name: /^done$/i }).click();

    const row = page.getByRole("row", { name: new RegExp(name) });
    await expect(row).toBeVisible();

    // Revoke -> confirmation dialog -> gone.
    await row.getByRole("button").last().click();
    await page.getByRole("alertdialog").getByRole("button", { name: /revoke/i }).click();
    await expect(page.getByRole("row", { name: new RegExp(name) })).toHaveCount(0);
  });

  test("org-scoped token reveals the org picker", async ({ page }) => {
    await page.goto("/tokens");
    await page.getByRole("button", { name: /new token/i }).click();
    await page.getByPlaceholder("laptop").fill(uniq("scoped-"));
    await chooseOption(page, "token-scope", "A single organization");
    await expect(page.getByTestId("token-org")).toBeVisible();
    await chooseOption(page, "token-org", "Acme Inc");
    await page.getByRole("button", { name: /^create$/i }).click();
    await expect(page.getByText(/copy it now/i)).toBeVisible();
  });
});
