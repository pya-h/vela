import { defineConfig } from "@playwright/test";

// End-to-end tests run the real vela-studio binary (debug build, which reads
// ide/web/dist from disk) against a copy of examples/, in Chromium. Run with
// `npm run e2e`, which builds everything first.
export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  timeout: 30_000,
  reporter: [["list"]],
  use: {
    browserName: "chromium",
    launchOptions: { executablePath: process.env["CHROMIUM"] ?? "/usr/bin/chromium" },
    trace: "retain-on-failure",
  },
});
