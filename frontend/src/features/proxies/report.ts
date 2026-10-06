import { useTranslation } from 'react-i18next'
import { toast } from '@/components/ui/toast'
import type { ReconcileReport } from './types'

/// 写操作顺带的固定分配对账：有 key 排队分不到代理时提示，别让人以为都分好了。
export function useReportToast() {
  const { t } = useTranslation()
  return (report?: ReconcileReport) => {
    if (report && report.unassigned > 0) {
      toast.warning(t('admin:egressUnassignedWarn', { n: report.unassigned }))
    } else {
      toast.success(t('common:success'))
    }
  }
}
