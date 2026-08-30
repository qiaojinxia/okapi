import { defineConfig } from '@playwright/test'

// e2e 冒烟（IMPLEMENTATION §13 M3 验收项）：打真实 console（API + 静态托管同源）。
// 前置：scripts/dev-deps.sh up && cargo build --bin okapi && pnpm build
export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  retries: 0,
  use: {
    baseURL: 'http://127.0.0.1:8081',
  },
  webServer: {
    command: 'cd .. && ./target/debug/okapi console',
    url: 'http://127.0.0.1:8081/healthz',
    reuseExistingServer: true,
    timeout: 30_000,
  },
})
