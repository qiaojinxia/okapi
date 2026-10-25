import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { SetWorkspace } from '@/features/compat/SetWorkspace'

/// 全局护栏：最后一层、对所有渠道生效，同 id 的规则能盖住厂商 / 档案 / 渠道层的写法。
/// 它是唯一的全局层，所以单独一个写权限点，草稿也要有发布权限的人批准。
export function GuardTab() {
  const { t } = useTranslation()
  return (
    <div className="flex flex-col gap-4">
      <Alert tone="warning" title={t('compat:guardTitle')}>
        {t('compat:guardHint')}
      </Alert>
      <SetWorkspace scope="guard" id={0} idPrefix="guard" />
    </div>
  )
}
