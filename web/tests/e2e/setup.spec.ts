import { test, expect } from "@playwright/test";

// The wizard itself ran in auth.setup; here we assert it's one-shot.
test.describe("first-run setup", () => {
  test("is complete and cannot be re-run", async ({ page }) => {
    const status = await page.request.get("/api/v1/setup/status");
    expect(status.ok()).toBeTruthy();
    expect((await status.json()).needs_setup).toBe(false);

    // Re-running setup is rejected once an admin exists.
    const redo = await page.request.post("/api/v1/setup", {
      data: {
        username: "intruder",
        email: "x@example.com",
        password: "password123",
        org_name: "X",
        org_slug: "x",
      },
    });
    expect(redo.status()).toBe(409);
  });

  test("authenticated root redirects into an org", async ({ page }) => {
    await page.goto("/");
    // Redirects to the newest org the admin belongs to (other specs create orgs).
    await expect(page).toHaveURL(/\/orgs\/[^/]+/);
  });
});
