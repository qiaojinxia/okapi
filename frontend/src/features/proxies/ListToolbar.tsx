import { useTranslation } from 'react-i18next'
import { Toolbar } from '@/components/ui/page'

/// 页签、条数与操作按钮同一行，表格紧跟其后（页头下不再单独占两行）。计数照页头的约定写成朴素文字。
export function ListToolbar({ tabs, total, children }: { tabs: React.ReactNode; total: number; children: React.ReactNode }) {
  const { t } = useTranslation()
  return (
    <Toolbar
      filters={<>{tabs}<span className="text-sm text-muted-foreground tabular-nums">{t('common:totalCount', { n: total })}</span></>}
      selection={children}
    />
  )
}
