import { defineConfig, devices } from "@playwright/test";

// The dashboard is served by the real `ruskery` binary (embedded SPA + API on
// one origin), started by tests/e2e/run-server.sh with a fresh temp DB, TLS off
// and a fast analytics rollup. Storage is lazy, so most specs need no S3.
const PORT = process.env.E2E_PORT || "8099";
const BASE_URL = process.env.E2E_BASE_URL || `http://127.0.0.1:${PORT}`;
const ADMIN_STATE = "tests/e2e/.auth/admin.json";

export default defineConfig({
  testDir: "./tests/e2e",
  // The backend is a single shared instance, so run serially for determinism.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : [["list"]],
  timeout: 30_000,
  expect: { timeout: 10_000 },

  use: {
    baseURL: BASE_URL,
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },

  // Build the UI + binary, then start the server (run-server.sh handles both).
  webServer: {
    command: "bash tests/e2e/run-server.sh",
    url: `${BASE_URL}/healthz`,
    // Off by default so tests never run against a stray backend on this port;
    // opt in explicitly during local iteration.
    reuseExistingServer: !!process.env.E2E_REUSE_SERVER,
    timeout: 180_000,
    stdout: "pipe",
    stderr: "pipe",
  },

  projects: [
    // Creates the first admin via the setup wizard + a non-admin, saving their
    // storage states for the browser projects to reuse.
    { name: "setup", testMatch: /auth\.setup\.ts/ },

    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], storageState: ADMIN_STATE },
      dependencies: ["setup"],
    },
    {
      name: "firefox",
      use: { ...devices["Desktop Firefox"], storageState: ADMIN_STATE },
      dependencies: ["setup"],
    },
    {
      name: "webkit",
      use: { ...devices["Desktop Safari"], storageState: ADMIN_STATE },
      dependencies: ["setup"],
    },
  ],
});
