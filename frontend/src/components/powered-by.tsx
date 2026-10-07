import { cn } from '@/lib/utils'

// 署名与原项目链接：README「许可证」依 AGPL-3.0 第 7 条要求修改版保留，不翻译、不随站点品牌替换
const OKAPI_REPO_URL = 'https://github.com/qiaojinxia/okapi'

export function PoweredBy({ className }: { className?: string }) {
  return (
    <p className={cn('text-xs text-muted-foreground', className)}>
      Powered by{' '}
      <a
        href={OKAPI_REPO_URL}
        target="_blank"
        rel="noopener noreferrer"
        className="font-medium underline-offset-4 hover:text-foreground hover:underline"
      >
        Okapi
      </a>
    </p>
  )
}
