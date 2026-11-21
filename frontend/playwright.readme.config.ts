import { defineConfig } from '@playwright/test'

// README 截图（e2e/readme-shots.spec.ts）：构建产物 + 接口桩 + 虚构演示数据，不需要后端。先 pnpm build。
export default defineConfig({
  testDir: './e2e',
  testMatch: ['readme-shots.spec.ts'],
  timeout: 60_000,
  workers: 1,
  use: {
    baseURL: 'http://127.0.0.1:4175',
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 2,
    timezoneId: 'Asia/Shanghai',
    locale: 'zh-CN',
  },
  webServer: {
    command: 'pnpm exec vite preview --host 127.0.0.1 --port 4175 --strictPort',
    url: 'http://127.0.0.1:4175',
    reuseExistingServer: false,
    timeout: 20_000,
  },
})
