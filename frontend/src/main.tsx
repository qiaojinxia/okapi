import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider, createRouter } from '@tanstack/react-router'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { registerAuthReset } from '@/lib/api'
import { initI18n } from '@/lib/i18n'
import { routeTree } from './routeTree.gen'
import './index.css'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { retry: 1, refetchOnWindowFocus: false },
  },
})

registerAuthReset(() => { void queryClient.cancelQueries(); queryClient.clear() })

const router = createRouter({ routeTree })

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
  interface StaticDataRouteOption {
    /// 页面要撑满视口、表格内部滚动（大表 + 常驻分页器）。由叶子路由自己声明，
    /// 布局层用 `useMatches` 读，不再硬编码一份路径清单。
    fitViewport?: boolean
  }
}

function mount(): void {
  const rootEl = document.getElementById('root')
  if (!rootEl) return
  createRoot(rootEl).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </StrictMode>,
  )
}

// 语言包是单独的块：下载完再挂 React，第一帧就是译好的文案。下载失败也照样挂（文案退成原始键），
// 页面至少能用、能刷新重试，失败原因留在控制台。
void initI18n().finally(mount)
