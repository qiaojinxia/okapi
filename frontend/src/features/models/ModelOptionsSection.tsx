import { ChevronDown } from 'lucide-react'
import type { ReactNode } from 'react'

/** Collapsing a section is presentation only; all draft values remain mounted. */
export function ModelOptionsSection({ id, title, hint, configured = false, children }: {
  id: string; title: string; hint?: string; configured?: boolean; children: ReactNode
}) {
  return <details id={id} className="rounded-xl border border-border bg-card [&[open]>summary>svg]:rotate-180">
    <summary className="flex cursor-pointer list-none items-center gap-3 rounded-xl px-4 py-2.5 outline-none hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-primary/40 [&::-webkit-details-marker]:hidden">
      {configured && <span aria-hidden className="h-1.5 w-1.5 shrink-0 rounded-full bg-primary" />}
      <span className="min-w-0 flex-1"><span className="block text-sm font-medium">{title}</span>
        {hint && <span className="mt-0.5 block text-xs leading-5 text-muted-foreground">{hint}</span>}</span>
      <ChevronDown aria-hidden className="h-4 w-4 shrink-0 text-muted-foreground transition-transform" />
    </summary>
    <div className="border-t border-border px-4 py-4">{children}</div>
  </details>
}
