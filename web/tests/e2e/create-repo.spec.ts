import { test, expect } from "@playwright/test";
import { uniq } from "./fixtures/helpers";

// Creating an empty repository is DB-only (no push), so these run without
// object storage. The setup wizard makes the admin the owner of `acme`.
test.describe("create repository (UI)", () => {
  test("creates an empty repo and it appears in the list", async ({ page }) => {
    const repo = uniq("ui");
    await page.goto("/orgs/acme");
    await page.getByTestId("new-repo").click();
    await page.getByTestId("new-repo-name").fill(repo);
    await page.getByTestId("new-repo-submit").click();
    await expect(page.getByRole("link", { name: repo })).toBeVisible();
  });

  test("rejects an invalid name", async ({ page }) => {
    await page.goto("/orgs/acme");
    await page.getByTestId("new-repo").click();
    await page.getByTestId("new-repo-name").fill("Invalid Name!");
    await page.getByTestId("new-repo-submit").click();
    await expect(page.getByText(/invalid repository name/i)).toBeVisible();
  });
});
