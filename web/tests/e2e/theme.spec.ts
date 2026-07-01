import { test, expect } from "@playwright/test";

test.describe("theme toggle (light/dark/system)", () => {
  test("switches theme and persists across reloads", async ({ page }) => {
    await page.goto("/tokens"); // authenticated page → sidebar (with the toggle)
    const html = page.locator("html");

    const pick = async (label: string) => {
      await page.getByRole("button", { name: /toggle theme/i }).click();
      await page.getByRole("menuitem").filter({ hasText: label }).click();
    };

    await pick("Dark");
    await expect(html).toHaveClass(/dark/);

    await pick("Light");
    await expect(html).not.toHaveClass(/dark/);

    // The preference is persisted (localStorage), so a reload keeps it.
    await page.reload();
    await expect(html).not.toHaveClass(/dark/);

    await pick("Dark");
    await expect(html).toHaveClass(/dark/);
    await page.reload();
    await expect(html).toHaveClass(/dark/);
  });
});
