import { test, expect } from "@playwright/test";
import { chooseOption } from "./fixtures/helpers";

test.describe("analytics (admin)", () => {
  test("renders overview cards and charts", async ({ page }) => {
    await page.goto("/orgs/acme/analytics");
    await expect(page.getByRole("heading", { name: "Analytics" })).toBeVisible();
    await expect(page.getByTestId("metric-pulls")).toBeVisible();
    await expect(page.getByTestId("metric-pushes")).toBeVisible();
    await expect(page.getByTestId("metric-storage")).toBeVisible();
    // Two charts (activity + storage) render as SVGs.
    await expect(page.getByRole("img")).toHaveCount(2);
  });

  test("range select refetches without error", async ({ page }) => {
    await page.goto("/orgs/acme/analytics");
    await chooseOption(page, "range", "Last 7 days");
    await expect(page.getByTestId("metric-pulls")).toBeVisible();
  });
});
