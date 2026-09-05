import { defineConfig } from '@playwright/test'

// 独立交互回归：使用构建产物与接口桩，无需数据库，也不修改演示账号或站点配置。
export default defineConfig({
  testDir: './e2e',
  testMatch: ['interactions.spec.ts', 'charts.spec.ts', 'catalog.spec.ts', 'request-examples.spec.ts'],
  timeout: 20_000,
  use: {
    baseURL: 'http://127.0.0.1:4175',
    viewport: { width: 1280, height: 800 },
    screenshot: 'only-on-failure',
  },
  webServer: {
    command: 'pnpm exec vite preview --host 127.0.0.1 --port 4175 --strictPort',
    url: 'http://127.0.0.1:4175',
    reuseExistingServer: false,
    timeout: 20_000,
  },
})
