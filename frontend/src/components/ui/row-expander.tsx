import { ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from './button'
import { cn } from '@/lib/utils'

export function RowExpander({ open, onToggle, controls, name }: {
  open: boolean; onToggle: () => void; controls: string; name: string
}) {
  const { t } = useTranslation()
  return <Button variant="ghost" size="icon" className="h-11 w-11 text-muted-foreground md:h-8 md:w-8"
    aria-label={t(open ? 'common:collapseRow' : 'common:expandRow', { name })}
    aria-expanded={open} aria-controls={open ? controls : undefined}
    onClick={(event) => { event.stopPropagation(); onToggle() }}>
    <ChevronRight aria-hidden className={cn('h-4 w-4 transition-transform', open && 'rotate-90')} />
  </Button>
}
