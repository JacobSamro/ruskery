import { test, expect } from "@playwright/test";
import { uniq } from "./fixtures/helpers";

test.describe("instance settings (admin)", () => {
  test("saves storage settings", async ({ page }) => {
    await page.goto("/settings");
    await expect(page.getByRole("heading", { name: /instance settings/i })).toBeVisible();
    await page.getByPlaceholder("my-registry-bucket").fill("e2e-bucket");
    await page.getByRole("button", { name: /save storage/i }).click();
    await expect(page.getByText(/saved/i)).toBeVisible();
  });

  test("requires a contact email before adding a domain", async ({ page }) => {
    await page.goto("/settings");
    // Clear any previously-saved contact email so the guard triggers (it checks
    // the field, which is pre-filled from the instance-wide saved value).
    await page.getByPlaceholder("admin@yourcompany.com").fill("");
    await page.getByPlaceholder("registry.yourcompany.com").fill(uniq("reg") + ".example.com");
    await page.getByRole("button", { name: /add domain/i }).click();
    await expect(page.getByText(/add a let's encrypt contact email/i)).toBeVisible();

    // Save a contact email, then the domain can be added (shows as pending).
    await page.getByPlaceholder("admin@yourcompany.com").fill("ops@example.com");
    const domain = uniq("reg") + ".example.com";
    await page.getByPlaceholder("registry.yourcompany.com").fill(domain);
    await page.getByRole("button", { name: /add domain/i }).click();
    await expect(page.getByText(domain)).toBeVisible();
    await expect(
      page.getByText(domain).locator("xpath=ancestor::li").getByText(/pending/i),
    ).toBeVisible();
  });

  test("shows the Google OAuth redirect URI", async ({ page }) => {
    await page.goto("/settings");
    await expect(page.getByText(/auth\/google\/callback/)).toBeVisible();
  });
});
