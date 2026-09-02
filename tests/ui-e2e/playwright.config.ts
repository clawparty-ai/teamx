import { defineConfig } from "@playwright/test"

export default defineConfig({
  testDir: ".",
  timeout: 45_000,
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: {
    browserName: "chromium",
    channel: process.env.TEAMX_UI_CHANNEL || "chrome",
    headless: true,
    viewport: { width: 1440, height: 900 },
    ignoreHTTPSErrors: true,
    trace: "off",
  },
})
