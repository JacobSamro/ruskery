import { expect, type APIRequestContext, type Page } from "@playwright/test";

/** Unique suffix so parallel/repeat runs don't collide on names. */
export const uniq = (prefix: string) =>
  `${prefix}${Date.now().toString(36)}${Math.floor(Math.random() * 1000)}`;

/** Open a shadcn-vue Select (by data-testid) and choose an option by its text. */
export async function chooseOption(page: Page, testid: string, optionText: string | RegExp) {
  await page.getByTestId(testid).click();
  await page.getByRole("option", { name: optionText }).click();
}

/** Create an org through the API using the current (admin) session. */
export async function apiCreateOrg(request: APIRequestContext, slug: string, name: string) {
  const r = await request.post("/api/v1/orgs", { data: { slug, name } });
  expect(r.ok(), `create org ${slug}`).toBeTruthy();
}
