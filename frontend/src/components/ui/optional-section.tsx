import type { ReactNode } from 'react'
import { ChevronRight } from 'lucide-react'

/** Optional fields remain mounted when folded, so folding never resets a draft. */
export function OptionalSection({ id, title, summary, hint, error, defaultOpen, onOpenChange, children }: {
  id: string
  title: string
  summary: string
  hint?: string
  error?: string
  /** Start expanded, e.g. when the section is the only content of its tab. */
  defaultOpen?: boolean
  onOpenChange?: (open: boolean) => void
  children: ReactNode
}) {
  return <section className="border-t border-border py-4 first:border-t-0 first:pt-0 last:pb-0">
    <details id={id} className="group/optional" open={defaultOpen || undefined}
      onToggle={(event) => onOpenChange?.(event.currentTarget.open)}>
      <summary tabIndex={0} className="flex cursor-pointer list-none items-start gap-2 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-primary/40 [&::-webkit-details-marker]:hidden">
        <ChevronRight aria-hidden className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground transition-transform group-open/optional:rotate-90" />
        <span className="flex min-w-0 flex-1 flex-wrap items-baseline gap-x-3 gap-y-1">
          <span className="text-sm font-medium">{title}</span>
          <span className="min-w-0 break-words text-xs leading-5 text-muted-foreground">{summary}</span>
        </span>
      </summary>
      <div className="mt-3 flex min-w-0 flex-col gap-3">
        {hint && <p className="text-xs leading-5 text-muted-foreground">{hint}</p>}
        {children}
      </div>
    </details>
    {error && <p role="alert" className="mt-2 text-xs leading-5 text-destructive">{error}</p>}
  </section>
}
