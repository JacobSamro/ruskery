import { test as setup, expect } from "@playwright/test";

// Runs once before the browser projects. Exercises the first-run wizard to
// create the admin, seeds a non-admin user, and saves both sessions as
// storageState for reuse.
const adminFile = "tests/e2e/.auth/admin.json";
const devFile = "tests/e2e/.auth/dev.json";

setup("create first admin via the setup wizard", async ({ page }) => {
  await page.goto("/setup");
  await page.getByTestId("setup-username").fill("admin");
  await page.getByTestId("setup-email").fill("admin@example.com");
  await page.getByTestId("setup-password").fill("supersecret");
  await page.getByTestId("setup-org-name").fill("Acme Inc");
  // Slug auto-fills from the name; pin it to a known value.
  await page.getByTestId("setup-org-slug").fill("acme");
  await page.getByRole("button", { name: /create & continue/i }).click();

  await expect(page).toHaveURL(/\/orgs\/acme/);
  await page.context().storageState({ path: adminFile });

  // Seed a non-admin user (admin session) for authz tests.
  const res = await page.request.post("/api/v1/users", {
    data: {
      username: "dev",
      email: "dev@example.com",
      password: "devpassword",
      is_admin: false,
    },
  });
  expect(res.ok()).toBeTruthy();
});

setup("save non-admin session", async ({ browser }) => {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto("/login");
  await page.getByTestId("login-username").fill("dev");
  await page.getByTestId("login-password").fill("devpassword");
  await page.getByRole("button", { name: /^sign in$/i }).click();
  await expect(page).not.toHaveURL(/\/login/);
  await ctx.storageState({ path: devFile });
  await ctx.close();
});
