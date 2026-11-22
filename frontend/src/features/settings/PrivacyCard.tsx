import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Database, MessageSquareText, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { useConfirm } from '@/components/ui/confirm'
import { Input, Label } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import {
  type ConversationArchivePolicy,
  DEFAULT_ARCHIVE_POLICY,
  fetchArchiveStats,
  runArchiveCleanup,
} from '@/features/logs/conversation-api'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { invalidateSetting } from '@/lib/invalidate'
import { qk } from '@/lib/query-keys'

const RETENTION_PRESETS = [7, 30, 90, 180, 365] as const
const BODY_KB_OPTIONS = [128, 256, 512, 1024, 2048, 4096] as const
const CLEANUP_HOURS_OPTIONS = [1, 6, 12, 24, 72] as const

/// 隐私与留痕：来源 IP 开关（settings.record_ip_log）+ 对话内容存档策略（settings.conversation_archive）。
export function PrivacyCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const current = useQuery({
    queryKey: qk.setting('record_ip_log'),
    queryFn: () => apiFetch<{ value: boolean | null }>('/admin/settings/record_ip_log'),
  })
  // 缺省（键不存在）= 记录：存量站点一直在记，缺省关掉会让日志无声地少一列
  const enabled = current.data?.value !== false

  const save = useMutation({
    mutationFn: (next: boolean) =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'record_ip_log', value: next },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      void invalidateSetting(queryClient, 'record_ip_log')
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardHeader>
          <CardTitle>{t('admin:privacyTitle')}</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <Switch
            label={t('admin:recordIpLog')}
            description={t('admin:recordIpLogHint')}
            checked={enabled}
            disabled={current.isPending || save.isPending}
            onChange={(next) => save.mutate(next)}
          />
        </CardContent>
      </Card>
      <ConversationArchiveCard />
    </div>
  )
}

function ConversationArchiveCard() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()

  const settingQuery = useQuery({
    queryKey: qk.setting('conversation_archive'),
    queryFn: () =>
      apiFetch<{ value: Partial<ConversationArchivePolicy> | null }>(
        '/admin/settings/conversation_archive',
      ),
  })

  const statsQuery = useQuery({
    queryKey: qk.adminConversationStats,
    queryFn: fetchArchiveStats,
    retry: false,
  })

  const [draft, setDraft] = useState<ConversationArchivePolicy>(DEFAULT_ARCHIVE_POLICY)
  const [daysInput, setDaysInput] = useState(String(DEFAULT_ARCHIVE_POLICY.retention_days))

  useEffect(() => {
    const raw = settingQuery.data?.value
    if (!raw || typeof raw !== 'object') {
      setDraft(DEFAULT_ARCHIVE_POLICY)
      setDaysInput(String(DEFAULT_ARCHIVE_POLICY.retention_days))
      return
    }
    const merged: ConversationArchivePolicy = {
      enabled: raw.enabled ?? DEFAULT_ARCHIVE_POLICY.enabled,
      retention_days: raw.retention_days ?? DEFAULT_ARCHIVE_POLICY.retention_days,
      allow_user_view: raw.allow_user_view ?? DEFAULT_ARCHIVE_POLICY.allow_user_view,
      max_body_kb: raw.max_body_kb ?? DEFAULT_ARCHIVE_POLICY.max_body_kb,
      cleanup_interval_hours:
        raw.cleanup_interval_hours ?? DEFAULT_ARCHIVE_POLICY.cleanup_interval_hours,
    }
    setDraft(merged)
    setDaysInput(merged.retention_days > 0 ? String(merged.retention_days) : '30')
  }, [settingQuery.data])

  const retentionMode: 'permanent' | 'scheduled' =
    draft.retention_days === 0 ? 'permanent' : 'scheduled'

  const parsedDays = /^\d+$/.test(daysInput.trim()) ? Number(daysInput.trim()) : Number.NaN
  const daysValid =
    retentionMode === 'permanent' ||
    (Number.isInteger(parsedDays) && parsedDays >= 1 && parsedDays <= 3650)

  const save = useMutation({
    mutationFn: () => {
      const payload: ConversationArchivePolicy = {
        ...draft,
        retention_days: retentionMode === 'permanent' ? 0 : parsedDays,
      }
      return apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'conversation_archive', value: payload },
      })
    },
    onSuccess: () => {
      toast.success(t('admin:saved'))
      void invalidateSetting(queryClient, 'conversation_archive')
      void queryClient.invalidateQueries({ queryKey: qk.adminConversations })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const cleanup = useMutation({
    mutationFn: runArchiveCleanup,
    onSuccess: (res) => {
      toast.success(
        t('admin:archiveCleanupDone', {
          pg: res.pg_deleted,
          days: res.retention_days,
        }),
      )
      void queryClient.invalidateQueries({ queryKey: qk.adminConversations })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const rawStats = statsQuery.data
  const stats =
    rawStats && typeof rawStats.total_count === 'number' && typeof rawStats.clickhouse_enabled === 'boolean'
      ? rawStats
      : undefined
  const activeRetentionDays = stats?.policy?.retention_days ?? draft.retention_days

  return (
    <Card data-slot="conversation-archive-card">
      {dialog}
      <CardHeader>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <MessageSquareText className="h-4 w-4 text-primary" />
            <CardTitle>{t('admin:archiveTitle')}</CardTitle>
          </div>
          {stats !== undefined && (
            <Badge
              data-slot="archive-backend-badge"
              variant={stats.clickhouse_enabled ? 'success' : 'info'}
              dot
            >
              <Database className="h-3 w-3" />
              {stats.clickhouse_enabled
                ? t('admin:archiveBackendClickhouse')
                : t('admin:archiveBackendPostgres')}
            </Badge>
          )}
        </div>
        <CardDescription>{t('admin:archiveDesc')}</CardDescription>
      </CardHeader>

      <CardContent className="flex flex-col gap-5">
        {/* 当前存储后端与归档概览条 */}
        {stats !== undefined && (
          <div
            data-slot="archive-stats-bar"
            className="flex flex-wrap items-center gap-x-4 gap-y-1.5 rounded-xl border border-border/80 bg-muted/25 px-3.5 py-2.5 text-xs text-muted-foreground tabular-nums"
          >
            <span>
              {t('admin:archiveTotalCount')}:{' '}
              <strong className="font-semibold text-foreground">{stats.total_count}</strong>
            </span>
            {stats.ch_count !== null && (
              <span>
                ClickHouse: <strong className="text-foreground">{stats.ch_count}</strong>
              </span>
            )}
            <span>
              PostgreSQL: <strong className="text-foreground">{stats.pg_count}</strong>
            </span>
            {stats.oldest_at !== null && (
              <span>
                {t('admin:archiveOldest')}:{' '}
                <strong className="font-mono text-foreground">
                  {new Date(stats.oldest_at).toLocaleDateString(i18n.language)}
                </strong>
              </span>
            )}
          </div>
        )}

        {/* 主开关 & 用户可见开关 */}
        <div className="flex flex-col gap-3.5">
          <Switch
            label={t('admin:archiveEnabled')}
            description={t('admin:archiveEnabledHint')}
            checked={draft.enabled}
            disabled={settingQuery.isPending || save.isPending}
            onChange={(enabled) => setDraft((d) => ({ ...d, enabled }))}
          />
          <Switch
            label={t('admin:archiveAllowUserView')}
            description={t('admin:archiveAllowUserViewHint')}
            checked={draft.allow_user_view}
            disabled={settingQuery.isPending || save.isPending}
            onChange={(allow_user_view) => setDraft((d) => ({ ...d, allow_user_view }))}
          />
        </div>

        {/* 保留策略：永久保留 vs 定时清理 */}
        <div className="flex flex-col gap-3 rounded-xl border border-border/75 bg-muted/20 p-3.5">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-col gap-0.5">
              <span className="text-sm font-medium text-foreground">
                {t('admin:archiveRetentionMode')}
              </span>
              <span className="text-xs text-muted-foreground">
                {retentionMode === 'permanent'
                  ? t('admin:archiveRetentionPermanentHint')
                  : t('admin:archiveRetentionScheduledHint')}
              </span>
            </div>
            <Segmented
              ariaLabel={t('admin:archiveRetentionMode')}
              value={retentionMode}
              onChange={(mode) => {
                if (mode === 'permanent') {
                  setDraft((d) => ({ ...d, retention_days: 0 }))
                } else {
                  const next = Number.isInteger(parsedDays) && parsedDays > 0 ? parsedDays : 30
                  setDaysInput(String(next))
                  setDraft((d) => ({ ...d, retention_days: next }))
                }
              }}
              options={[
                { value: 'permanent', label: t('admin:archivePermanent') },
                { value: 'scheduled', label: t('admin:archiveScheduled') },
              ]}
            />
          </div>

          {retentionMode === 'scheduled' && (
            <div className="flex flex-col gap-3 border-t border-border/60 pt-3">
              <div className="flex flex-wrap items-center gap-1.5">
                {RETENTION_PRESETS.map((d) => (
                  <Button
                    key={d}
                    type="button"
                    size="sm"
                    variant={parsedDays === d ? 'default' : 'outline'}
                    className="h-7 px-2.5 text-xs"
                    onClick={() => {
                      setDaysInput(String(d))
                      setDraft((prev) => ({ ...prev, retention_days: d }))
                    }}
                  >
                    {t('admin:archiveDaysPreset', { days: d })}
                  </Button>
                ))}
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="archive-retention-days">{t('admin:archiveRetentionDays')}</Label>
                  <Input
                    id="archive-retention-days"
                    inputMode="numeric"
                    value={daysInput}
                    aria-invalid={!daysValid}
                    onChange={(e) => {
                      setDaysInput(e.target.value)
                      const n = Number(e.target.value.trim())
                      if (Number.isInteger(n) && n >= 1 && n <= 3650) {
                        setDraft((prev) => ({ ...prev, retention_days: n }))
                      }
                    }}
                  />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="archive-cleanup-interval">
                    {t('admin:archiveCleanupInterval')}
                  </Label>
                  <Select
                    id="archive-cleanup-interval"
                    value={String(draft.cleanup_interval_hours)}
                    onChange={(v) =>
                      setDraft((prev) => ({ ...prev, cleanup_interval_hours: Number(v) }))
                    }
                    options={CLEANUP_HOURS_OPTIONS.map((h) => ({
                      value: String(h),
                      label: t('admin:archiveEveryHours', { hours: h }),
                    }))}
                  />
                </div>
              </div>
            </div>
          )}
        </div>

        {/* 体积上限与媒体剥离说明 */}
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="archive-max-kb">{t('admin:archiveMaxBodyKb')}</Label>
            <Select
              id="archive-max-kb"
              value={String(draft.max_body_kb)}
              onChange={(v) => setDraft((prev) => ({ ...prev, max_body_kb: Number(v) }))}
              options={BODY_KB_OPTIONS.map((kb) => ({
                value: String(kb),
                label: kb >= 1024 ? `${kb / 1024} MB` : `${kb} KB`,
              }))}
            />
            <p className="text-xs text-muted-foreground">{t('admin:archiveMaxBodyHint')}</p>
          </div>
        </div>

        {/* 操作栏：保存 + 立即清理 */}
        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border/70 pt-3">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={cleanup.isPending || activeRetentionDays === 0}
            onClick={() =>
              confirm({
                title: t('admin:archiveCleanupConfirmTitle'),
                description: t('admin:archiveCleanupConfirmDesc', {
                  days: activeRetentionDays,
                }),
                confirmLabel: t('admin:archiveCleanupNow'),
                onConfirm: () => cleanup.mutate(),
              })
            }
          >
            <Trash2 className="h-3.5 w-3.5" />
            {t('admin:archiveCleanupNow')}
          </Button>
          <Button
            type="button"
            loading={save.isPending}
            disabled={settingQuery.isPending || !daysValid}
            onClick={() => save.mutate()}
          >
            {t('common:save')}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
