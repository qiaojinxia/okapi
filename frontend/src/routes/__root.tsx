import { Outlet, createRootRoute } from '@tanstack/react-router'
import { Toaster } from '@/components/ui/toast'

export const Route = createRootRoute({
  component: () => (
    <>
      <Outlet />
      {/* 全局消息层挂在路由根：登录页与门户/后台共用一套反馈 */}
      <Toaster />
    </>
  ),
})
