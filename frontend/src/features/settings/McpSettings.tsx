import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight, Bot, Check, Copy, Plug, ShieldCheck } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { useConfirm } from '@/components/ui/confirm'
import { CopyButton, useCopy } from '@/components/ui/copy-button'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { McpConnectionError, mcpConfig, testMcpConnection } from './mcp-connection'
import { isRecord } from './setting-catalog'
import type { SettingRow } from './setting-catalog'

const PROMPTS = [
  { id: 'inspect', label: 'admin:mcpPromptInspect', content: 'admin:mcpPromptInspectText' },
  { id: 'usage', label: 'admin:mcpPromptUsage', content: 'admin:mcpPromptUsageText' },
  { id: 'manage', label: 'admin:mcpPromptManage', content: 'admin:mcpPromptManageText' },
] as const
const CONNECTION_ERRORS = {
  auth: 'admin:mcpErrorAuth', forbidden: 'admin:mcpErrorForbidden', unavailable: 'admin:mcpErrorUnavailable',
  response: 'admin:mcpErrorResponse', timeout: 'admin:mcpErrorTimeout', network: 'admin:mcpErrorNetwork',
}

export function McpSettings() {
  const { t } = useTranslation()
  const can = usePermission()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()
  const [promptId, setPromptId] = useState<string>('inspect')
  const endpoint = new URL('/mcp', window.location.origin).href
  // 单项读取接口要求 settings.write；只读用户应使用已脱敏的列表接口。
  const current = useQuery({
    queryKey: qk.adminSettings,
    queryFn: () => apiFetch<{ data: SettingRow[] }>('/admin/settings'),
  })
  const rows = current.data?.data
  const row = Array.isArray(rows) ? rows.find((item) => isRecord(item) && item.key === 'mcp_write_enabled') : undefined
  const value = row ? row.value : null
  const known = current.isSuccess && Array.isArray(rows) && (value === null || typeof value === 'boolean')
  const enabled = known && value === true
  const connection = useMutation({ mutationFn: testMcpConnection })
  const save = useMutation({
    mutationFn: (value: boolean) => apiFetch('/admin/settings', { method: 'POST', body: { key: 'mcp_write_enabled', value } }),
    onSuccess: async () => {
      connection.reset()
      toast.success(t('common:saved'))
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: qk.setting('mcp_write_enabled') }),
        queryClient.invalidateQueries({ queryKey: qk.adminSettings }),
      ])
    },
    onError: (error) => toast.error(describeError(error)),
  })
  const prompt = `${t('admin:mcpPromptBase')}\n\n${t(PROMPTS.find((item) => item.id === promptId)!.content)}`

  return <div className="flex min-w-0 flex-col gap-4">
    <div className="flex flex-wrap items-center gap-3 rounded-lg border border-primary/20 bg-primary/5 p-4">
      <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary"><Bot aria-hidden className="h-5 w-5" /></span>
      <div className="min-w-0 flex-1"><h2 className="text-sm font-semibold">{t('admin:mcpHeading')}</h2><p className="mt-1 text-xs leading-5 text-muted-foreground">{t('admin:mcpDescription')}</p></div>
      <Badge variant={!known ? 'muted' : enabled ? 'warning' : 'success'} dot>{t(!known ? 'admin:mcpStateUnknown' : enabled ? 'admin:mcpWriteEnabled' : 'admin:mcpReadOnly')}</Badge>
    </div>

    <div className="grid min-w-0 items-start gap-4 xl:grid-cols-[minmax(0,1.4fr)_minmax(340px,1fr)]">
      <div className="flex min-w-0 flex-col gap-4">
        <Card className="min-w-0">
          <CardHeader><CardTitle>{t('admin:mcpConnectTitle')}</CardTitle><CardDescription>{t('admin:mcpConnectHint')}</CardDescription></CardHeader>
          <CardContent className="flex min-w-0 flex-col gap-4">
            <Field label={t('admin:mcpEndpoint')} htmlFor="mcp-endpoint" hint={t('admin:mcpEndpointHint')}>
              <div className="flex items-center gap-2"><Input id="mcp-endpoint" className="min-w-0 font-mono text-xs" value={endpoint} readOnly /><CopyButton value={endpoint} label={t('admin:mcpCopyEndpoint')} /></div>
            </Field>
            <div className="flex flex-wrap gap-2"><Badge variant="outline">Streamable HTTP</Badge><Badge variant="outline">Bearer API Key</Badge></div>
            <CopyBlock title={t('admin:mcpConfigTitle')} label={t('admin:mcpCopyConfig')} value={mcpConfig(endpoint)} />
            <p className="text-xs leading-5 text-muted-foreground">{t('admin:mcpConfigHint')}</p>
            <Alert tone="warning"><p className="text-xs leading-5">{t('admin:mcpKeyWarning')}</p></Alert>
            <Link to="/portal/keys" className="inline-flex items-center gap-1 self-start text-xs text-primary hover:underline">{t('admin:mcpKeysLink')}<ArrowUpRight aria-hidden className="h-3.5 w-3.5" /></Link>
          </CardContent>
        </Card>
        <Card className="min-w-0">
          <CardHeader><CardTitle>{t('admin:mcpPromptTitle')}</CardTitle><CardDescription>{t('admin:mcpPromptHint')}</CardDescription></CardHeader>
          <CardContent className="flex min-w-0 flex-col gap-3">
            <Field label={t('admin:mcpTask')} htmlFor="mcp-task" className="sm:max-w-xs">
              <Select id="mcp-task" value={promptId} onChange={setPromptId} options={PROMPTS.map((item) => ({ value: item.id, label: t(item.label) }))} />
            </Field>
            <CopyBlock title={t('admin:mcpPromptPreview')} label={t('admin:mcpCopyPrompt')} value={prompt} prose />
          </CardContent>
        </Card>
      </div>

      <div className="flex min-w-0 flex-col gap-4">
        <Card>
          <CardHeader><CardTitle className="flex items-center gap-2"><ShieldCheck aria-hidden className="h-4 w-4 text-primary" />{t('admin:mcpPermissionsTitle')}</CardTitle><CardDescription>{t('admin:mcpPermissionsHint')}</CardDescription></CardHeader>
          <CardContent className="flex flex-col gap-4">
            {current.isError ? <ErrorState message={describeError(current.error)} onRetry={() => void current.refetch()} />
              : current.isSuccess && !known ? <ErrorState message={t('admin:mcpErrorResponse')} onRetry={() => void current.refetch()} />
              : <Switch label={t('admin:settingMcpWrite')} description={t('admin:mcpWriteHint')} checked={enabled} disabled={!known || current.isFetching || save.isPending || connection.isPending || !can('settings.write')}
                onChange={(next) => confirm({
                  title: t(next ? 'admin:mcpEnableTitle' : 'admin:mcpDisableTitle'),
                  description: t(next ? 'admin:mcpEnableWarning' : 'admin:mcpDisableWarning'),
                  confirmLabel: t(next ? 'admin:mcpEnableConfirm' : 'admin:mcpDisableConfirm'),
                  tone: next ? 'destructive' : 'default',
                  onConfirm: () => save.mutate(next),
                })} />}
            <div className="space-y-2 border-t border-border pt-3 text-xs leading-5 text-muted-foreground"><p>{t('admin:mcpPermissionBoundary')}</p><p>{t('admin:mcpApprovalWarning')}</p></div>
            {can('audit.read') && <Link to="/admin/audit" className="inline-flex items-center gap-1 self-start text-xs text-primary hover:underline">{t('admin:mcpAuditLink')}<ArrowUpRight aria-hidden className="h-3.5 w-3.5" /></Link>}
          </CardContent>
        </Card>
        <Card className="min-w-0">
          <CardHeader><CardTitle>{t('admin:mcpTestTitle')}</CardTitle><CardDescription>{t('admin:mcpTestHint')}</CardDescription></CardHeader>
          <CardContent className="flex min-w-0 flex-col gap-3">
            <Button className="self-start" variant="outline" loading={connection.isPending} disabled={save.isPending} onClick={() => connection.mutate()}><Plug aria-hidden className="h-4 w-4" />{t('admin:mcpTest')}</Button>
            {connection.isError && <Alert tone="destructive" title={t('admin:mcpTestFailed')}><p className="text-xs leading-5">{t(CONNECTION_ERRORS[connection.error instanceof McpConnectionError ? connection.error.reason : 'network'])}</p></Alert>}
            {connection.isSuccess && <>
              <Alert tone="success" title={t('admin:mcpTestSuccess')}><p className="break-words text-xs leading-5">{connection.data.server} · {connection.data.version} · {connection.data.protocol}</p><p className="text-xs leading-5">{t('admin:mcpToolCount', { n: connection.data.tools.length })}</p></Alert>
              <details className="min-w-0 rounded-md border border-border p-3"><summary className="cursor-pointer text-xs font-medium">{t('admin:mcpToolList')}</summary><ul className="mt-3 max-h-64 space-y-3 overflow-y-auto">{connection.data.tools.map((tool, index) => <li key={`${tool.name}-${index}`} className="break-words text-xs"><code className="font-medium break-all">{tool.name}</code><p className="mt-1 leading-5 text-muted-foreground">{tool.description}</p></li>)}</ul></details>
            </>}
            <p className="text-xs leading-5 text-muted-foreground">{t('admin:mcpTestScope')}</p>
          </CardContent>
        </Card>
        <Alert title={t('admin:mcpReachabilityTitle')}><p className="text-xs leading-5">{t('admin:mcpReachabilityHint')}</p></Alert>
      </div>
    </div>
    {dialog}
  </div>
}

function CopyBlock({ title, label, value, prose = false }: { title: string; label: string; value: string; prose?: boolean }) {
  const { copy, copied } = useCopy()
  return <section aria-label={title} className="min-w-0 overflow-hidden rounded-lg border border-border">
    <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-muted/50 px-3 py-2"><h4 className="text-xs font-medium text-muted-foreground">{title}</h4><Button variant="ghost" size="xs" onClick={() => void copy(value)}>{copied ? <Check aria-hidden className="h-3.5 w-3.5 text-success" /> : <Copy aria-hidden className="h-3.5 w-3.5" />}{label}</Button></div>
    <pre className={`max-h-80 overflow-auto p-3 text-xs leading-6 whitespace-pre-wrap break-words ${prose ? 'font-sans' : 'font-mono'}`}>{value}</pre>
  </section>
}
