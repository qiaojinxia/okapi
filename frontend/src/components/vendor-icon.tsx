import { Boxes } from 'lucide-react'
import { cn } from '@/lib/utils'

/// 厂商标志：有图标用图标，没有就取名字首字。`vendor` 只读名字与图标，模型目录的厂商对象直接传进来即可。
export function VendorIcon({ vendor, size = 'md' }: { vendor: { name: string; icon?: string }; size?: 'sm' | 'md' | 'lg' }) {
  return <span aria-hidden className={cn('inline-flex shrink-0 items-center justify-center rounded-xl border border-border bg-card', vendor.icon?.endsWith('-color') && 'dark:bg-foreground/95',
    size === 'sm' ? 'h-8 w-8 rounded-lg' : size === 'lg' ? 'h-14 w-14' : 'h-11 w-11')}>
    {vendor.icon ? <img src={`/vendor-icons/${vendor.icon}.svg`} alt="" width={24} height={24}
      className={cn(size === 'sm' ? 'h-5 w-5' : size === 'lg' ? 'h-8 w-8' : 'h-6 w-6', !vendor.icon.endsWith('-color') && 'dark:invert')} />
      : vendor.name ? <span className="text-sm font-semibold text-muted-foreground">{Array.from(vendor.name)[0].toUpperCase()}</span>
        : <Boxes className="h-5 w-5 text-muted-foreground" />}
  </span>
}
