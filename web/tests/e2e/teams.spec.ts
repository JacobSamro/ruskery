import { test, expect } from "@playwright/test";
import { uniq } from "./fixtures/helpers";

test.describe("teams (admin)", () => {
  test("create a team and add a member", async ({ page }) => {
    const name = uniq("Team-");
    const slug = name.toLowerCase();
    await page.goto("/orgs/acme/teams");

    await page.getByRole("button", { name: /new team/i }).click();
    await page.getByPlaceholder("Backend", { exact: true }).fill(name);
    await page.getByPlaceholder("backend", { exact: true }).fill(slug);
    await page.getByRole("button", { name: /^create$/i }).click();

    const teamEntry = page.getByText(name, { exact: true });
    await expect(teamEntry).toBeVisible();
    await teamEntry.click();

    // Add the seeded 'dev' user to the team.
    await page.getByPlaceholder("username").fill("dev");
    await page.getByRole("button", { name: /^add$/i }).click();
    await expect(page.getByText("dev", { exact: false })).toBeVisible();
  });
});
