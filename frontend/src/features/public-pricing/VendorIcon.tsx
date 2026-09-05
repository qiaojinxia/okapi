import { Boxes } from 'lucide-react'
import type { Vendor } from './catalog-data'
import { cn } from '@/lib/utils'

export function VendorIcon({ vendor, size = 'md' }: { vendor: Vendor; size?: 'sm' | 'md' | 'lg' }) {
  return <span aria-hidden className={cn('inline-flex shrink-0 items-center justify-center rounded-xl border border-border bg-card', vendor.icon?.endsWith('-color') && 'dark:bg-foreground/95',
    size === 'sm' ? 'h-8 w-8 rounded-lg' : size === 'lg' ? 'h-14 w-14' : 'h-11 w-11')}>
    {vendor.icon ? <img src={`/vendor-icons/${vendor.icon}.svg`} alt="" width={24} height={24}
      className={cn(size === 'sm' ? 'h-5 w-5' : size === 'lg' ? 'h-8 w-8' : 'h-6 w-6', !vendor.icon.endsWith('-color') && 'dark:invert')} />
      : vendor.name ? <span className="text-sm font-semibold text-muted-foreground">{Array.from(vendor.name)[0].toUpperCase()}</span>
        : <Boxes className="h-5 w-5 text-muted-foreground" />}
  </span>
}
