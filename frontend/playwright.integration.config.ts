import { defineConfig } from '@playwright/test'

if (process.env.OKAPI_TEST_ISOLATED !== '1' || !new URL(process.env.DATABASE_URL ?? '').pathname.startsWith('/okapi_test_')) {
  throw new Error('Real API E2E requires disposable resources from scripts/test-isolated.py')
}
const port = 48081
export default defineConfig({
  testDir: './e2e', testMatch: ['smoke.spec.ts', 'screenshots.spec.ts'], timeout: 30_000, retries: 0,
  use: { baseURL: `http://127.0.0.1:${port}` },
  webServer: {
    command: `cd .. && OKAPI_CONSOLE_BIND=127.0.0.1:${port} ./target/debug/okapi console`,
    url: `http://127.0.0.1:${port}/healthz`, reuseExistingServer: false, timeout: 30_000,
  },
})
