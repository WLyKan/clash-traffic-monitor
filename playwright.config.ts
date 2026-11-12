import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests-ui', testMatch: '**/*.spec.ts', fullyParallel: false, workers: 1,
  outputDir: process.env.CLASH_UI_ARTIFACTS ?? 'test-results',
  reporter: 'list', timeout: 30_000,
  use: { baseURL: 'http://127.0.0.1:1420', headless: true, viewport: { width: 1280, height: 850 },
    screenshot: 'only-on-failure', trace: 'retain-on-failure' },
  webServer: { command: 'npm run dev', url: 'http://127.0.0.1:1420', reuseExistingServer: !process.env.CI },
});
