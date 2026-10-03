import { defineConfig } from '@playwright/test';

// End-to-end tests against a real `amail serve` (and `amail node`) started by
// e2e/global-setup.ts. Browsers are installed into .e2e/browsers:
//   PLAYWRIGHT_BROWSERS_PATH=.e2e/browsers npx playwright install chromium
process.env.PLAYWRIGHT_BROWSERS_PATH ??= new URL('./.e2e/browsers', import.meta.url).pathname;

export default defineConfig({
  testDir: 'e2e',
  outputDir: '.e2e/test-results',
  globalSetup: './e2e/global-setup.ts',
  fullyParallel: false,
  workers: 1,
  timeout: 90_000,
  expect: { timeout: 10_000 },
  reporter: [['list']],
  use: {
    browserName: 'chromium',
    viewport: { width: 1440, height: 900 },
    trace: 'retain-on-failure',
  },
});
