import { test, expect } from "@playwright/test";
import { uniq, apiCreateOrg } from "./fixtures/helpers";

test.describe("organizations (admin)", () => {
  test("lists all orgs and can create one", async ({ page }) => {
    await page.goto("/orgs");
    await expect(page.getByRole("heading", { name: "Organizations" })).toBeVisible();
    // The acme org's slug appears in the listing (sidebar shows names, not slugs).
    await expect(page.getByText("acme", { exact: true })).toBeVisible();

    await page.getByRole("button", { name: /new organization/i }).click();
    const name = uniq("Team ");
    const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
    await page.getByPlaceholder("Acme Inc").fill(name);
    await page.getByPlaceholder("acme", { exact: true }).fill(slug);
    await page.getByRole("button", { name: /^create$/i }).click();

    await expect(page.getByText(slug, { exact: true })).toBeVisible();
  });

  test("org switcher navigates between orgs", async ({ page, request }) => {
    const slug = uniq("sw");
    const name = `Switch ${slug}`; // unique name so the option is unambiguous
    await apiCreateOrg(request, slug, name);

    await page.goto("/orgs/acme");
    await page.getByTestId("org-switcher").click();
    await page.getByRole("option", { name }).click();
    await expect(page).toHaveURL(new RegExp(`/orgs/${slug}`));
  });
});
