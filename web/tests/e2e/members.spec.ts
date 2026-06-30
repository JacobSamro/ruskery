import { test, expect } from "@playwright/test";

test.describe("members (admin)", () => {
  test("add, change role, and remove a member", async ({ page }) => {
    await page.goto("/orgs/acme/members");

    // Add the seeded 'dev' user.
    await page.getByRole("button", { name: /add member/i }).click();
    await page.getByPlaceholder("existing user").fill("dev");
    await page.getByRole("button", { name: /^add$/i }).click();

    const row = page.getByRole("row", { name: /dev/ });
    await expect(row).toBeVisible();

    // Change their role via the inline shadcn Select.
    await row.getByRole("combobox").click();
    await page.getByRole("option", { name: "admin" }).click();
    await expect(row.getByRole("combobox")).toContainText("admin");

    // Remove -> confirmation dialog -> gone.
    await row.getByRole("button").last().click();
    await page.getByRole("alertdialog").getByRole("button", { name: /remove/i }).click();
    await expect(page.getByRole("row", { name: /dev/ })).toHaveCount(0);
  });
});
