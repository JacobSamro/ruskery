import { test, expect } from "@playwright/test";
import { trySeedImage } from "./fixtures/seed-image";
import { uniq } from "./fixtures/helpers";

// These need working object storage (push). When it's absent (e.g. a Tier-A
// run with no RustFS/MinIO), seeding fails and the tests skip cleanly.
test.describe("repositories (needs object storage)", () => {
  test("browse a seeded repo and its tag", async ({ page, request }) => {
    const seeded = await trySeedImage(request, "acme", "webapp", "v1");
    test.skip(!seeded, "object storage not available");

    await page.goto("/orgs/acme");
    await page.getByRole("link", { name: "webapp" }).click();
    await expect(page).toHaveURL(/\/orgs\/acme\/repos\/webapp/);
    await expect(page.getByText("v1")).toBeVisible();
    await expect(page.getByText(/docker pull/)).toBeVisible();
  });

  test("delete a repo with confirmation", async ({ page, request }) => {
    const repo = uniq("del");
    const seeded = await trySeedImage(request, "acme", repo, "v1");
    test.skip(!seeded, "object storage not available");

    await page.goto(`/orgs/acme/repos/${repo}`);
    await page.getByRole("button", { name: /delete/i }).click();
    await page.getByRole("alertdialog").getByRole("button", { name: /delete/i }).click();
    await expect(page).toHaveURL(/\/orgs\/acme$/);
  });

  test("analytics reflects a seeded push", async ({ page, request }) => {
    const repo = uniq("an");
    const seeded = await trySeedImage(request, "acme", repo, "v1");
    test.skip(!seeded, "object storage not available");

    await page.waitForTimeout(2000); // let the 1s analytics rollup flush
    await page.goto("/orgs/acme/analytics");
    // The pushed repo shows up in the Top repositories table.
    await expect(page.getByText(repo)).toBeVisible();
  });
});
