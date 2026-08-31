import { tanstackRouter } from '@tanstack/router-plugin/vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vite'
import { fileURLToPath } from 'node:url'

// dev 代理指向 console 角色（OKAPI_CONSOLE_BIND 缺省 127.0.0.1:8081）
const CONSOLE = 'http://127.0.0.1:8081'

export default defineConfig({
  plugins: [
    tanstackRouter({ target: 'react', autoCodeSplitting: true }),
    react(),
    tailwindcss(),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    proxy: {
      '/api': CONSOLE,
      '/admin': CONSOLE,
      '/auth': CONSOLE,
      '/pay': CONSOLE,
      '/mcp': CONSOLE,
      '/healthz': CONSOLE,
    },
  },
})
