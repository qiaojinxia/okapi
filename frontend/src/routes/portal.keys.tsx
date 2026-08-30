import { useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/portal/keys')({
  component: KeysPage,
})

interface KeyRow {
  id: number
  name: string
  key_prefix: string
  status: number
  used_micro: number
  rpm_limit: number | null
  created_at: string
  amount_micro: number
  requests: number
}

function KeysPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const keys = useQuery({
    queryKey: qk.keys,
    queryFn: () => apiFetch<{ data: KeyRow[] }>('/api/me/keys'),
  })

  if (keys.isError) {
    return <p className="text-sm text-destructive">{describeError(keys.error)}</p>
  }
  return (
    <Table>
      <THead>
        <Tr>
          <Th>{t('portal:keyName')}</Th>
          <Th>{t('portal:keyPrefix')}</Th>
          <Th>{t('common:status')}</Th>
          <Th>{t('portal:keyUsed')}</Th>
          <Th>{t('portal:keyRequests')}</Th>
          <Th>{t('portal:keyRpm')}</Th>
          <Th>{t('portal:keyCreated')}</Th>
        </Tr>
      </THead>
      <TBody>
        {(keys.data?.data ?? []).map((k) => (
          <Tr key={k.id}>
            <Td>{k.name}</Td>
            <Td className="font-mono text-xs">{k.key_prefix}…</Td>
            <Td>
              <Badge variant={k.status === 1 ? 'success' : 'muted'}>
                {k.status === 1 ? t('common:enabled') : t('common:disabled')}
              </Badge>
            </Td>
            <Td>{formatMoney(k.amount_micro || k.used_micro, locale)}</Td>
            <Td>{formatCount(k.requests, locale)}</Td>
            <Td>{k.rpm_limit ?? '—'}</Td>
            <Td>{dayjs(k.created_at).format('YYYY-MM-DD')}</Td>
          </Tr>
        ))}
      </TBody>
    </Table>
  )
}
