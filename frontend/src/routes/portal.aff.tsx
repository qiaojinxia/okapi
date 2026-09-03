import { useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { Coins, Gift, Link2, Users } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton, useCopy } from '@/components/ui/copy-button'
import { ErrorState } from '@/components/ui/state'
import { PageHeader } from '@/components/ui/page'
import { Skeleton } from '@/components/ui/skeleton'
import { Stat } from '@/components/ui/stat'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

export const Route = createFileRoute('/portal/aff')({
  component: AffPage,
})

interface AffInfo {
  aff_code: string
  invitees: number
  reward_sum_micro: number
}

/// 邀请返利：链接放在最显眼的位置（这页唯一的动作就是"把链接发出去"），
/// 邀请人数与累计返利两张数字卡在右侧作反馈。
function AffPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const { copy } = useCopy()
  const aff = useQuery({
    queryKey: ['me-aff'],
    queryFn: () => apiFetch<AffInfo>('/api/me/aff'),
  })

  const link = aff.data ? `${window.location.origin}/?aff=${aff.data.aff_code}` : ''

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('portal:affTitle')} description={t('portal:affHint')} icon={Gift} />

      {aff.isError && <ErrorState message={describeError(aff.error)} onRetry={() => void aff.refetch()} />}

      <div className="grid gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Link2 className="h-4 w-4 text-primary" />
              {t('portal:affLinkTitle')}
            </CardTitle>
            <CardDescription>{t('portal:affLinkHint')}</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4 pt-3">
            {aff.isPending ? (
              <>
                <Skeleton className="h-11 w-full" />
                <Skeleton className="h-9 w-40" />
              </>
            ) : aff.data ? (
              <>
                <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/40 p-3">
                  <code className="min-w-0 flex-1 font-mono text-xs break-all">{link}</code>
                  <CopyButton value={link} label={t('portal:affCopyLink')} />
                </div>
                <div className="flex flex-wrap items-center gap-3">
                  <Button onClick={() => void copy(link)}>
                    <Link2 className="h-4 w-4" />
                    {t('portal:affCopyLink')}
                  </Button>
                  <span className="text-xs text-muted-foreground">
                    {t('portal:affCodeLabel')}{' '}
                    <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-foreground">
                      {aff.data.aff_code}
                    </code>
                  </span>
                </div>
              </>
            ) : null}
          </CardContent>
        </Card>

        <div className="grid grid-cols-2 gap-3 self-start lg:grid-cols-1">
          <Stat
            icon={Users}
            label={t('portal:affInvitees')}
            loading={aff.isPending}
            value={aff.data?.invitees ?? 0}
          />
          <Stat
            icon={Coins}
            label={t('portal:affReward')}
            loading={aff.isPending}
            value={formatMoney(aff.data?.reward_sum_micro ?? 0, locale)}
            tone={(aff.data?.reward_sum_micro ?? 0) > 0 ? 'good' : 'default'}
          />
        </div>
      </div>
    </div>
  )
}
