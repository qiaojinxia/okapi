import { useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
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

function AffPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [copied, setCopied] = useState(false)
  const aff = useQuery({
    queryKey: ['me-aff'],
    queryFn: () => apiFetch<AffInfo>('/api/me/aff'),
  })

  const link = aff.data
    ? `${window.location.origin}/?aff=${aff.data.aff_code}`
    : ''

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('portal:affTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <p className="text-xs text-muted-foreground">{t('portal:affHint')}</p>
        {aff.isError && (
          <p className="text-xs text-muted-foreground">{describeError(aff.error)}</p>
        )}
        {aff.data && (
          <>
            <div className="flex items-center gap-3">
              <code className="rounded-md border px-3 py-1.5 font-mono text-sm">
                {aff.data.aff_code}
              </code>
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  void navigator.clipboard.writeText(link)
                  setCopied(true)
                  setTimeout(() => setCopied(false), 1500)
                }}
              >
                {copied ? t('portal:affCopied') : t('portal:affCopyLink')}
              </Button>
            </div>
            <div className="grid grid-cols-2 gap-3 sm:max-w-md">
              <div className="rounded-md border p-3">
                <div className="text-xs text-muted-foreground">{t('portal:affInvitees')}</div>
                <div className="mt-1 text-xl font-semibold">{aff.data.invitees}</div>
              </div>
              <div className="rounded-md border p-3">
                <div className="text-xs text-muted-foreground">{t('portal:affReward')}</div>
                <div className="mt-1 text-xl font-semibold">
                  {formatMoney(aff.data.reward_sum_micro, locale)}
                </div>
              </div>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  )
}
