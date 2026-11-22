import {
  Activity,
  Copy,
  Ellipsis,
  ExternalLink,
  Gauge,
  Globe,
  KeyRound,
  Loader2,
  MessageSquareText,
  Pencil,
  Pin,
  Trash2,
  Wallet,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { ActionGroup, ICON_BUTTON_CLASS, IconButton } from '@/components/ui/icon-button'
import { Menu, MenuItem, MenuSeparator } from '@/components/ui/menu'
import { Switch } from '@/components/ui/switch'
import { Tooltip } from '@/components/ui/tooltip'
import { ChannelQuotaCell } from '@/features/channels/account-controls/ChannelQuotaCell'
import type { ChannelEditFocus } from '@/features/channels/ChannelDrawer'
import {
  ChannelUsage,
  Health24h,
  KeyStateDetail,
  KeyStateSummary,
  LastBalance,
  LastProbe,
  type ChannelUsageRow,
} from '@/features/channels/ChannelHealthCell'
import { ProviderChip, ProviderMark } from '@/features/channels/ProviderChip'
import {
  balanceSupported,
  costMilliToRatio,
  providerConsoleUrl,
  type ChannelRow,
} from '@/features/channels/types'
import type { ChannelRow as ChannelStatRow } from '@/features/stats/types'
import { maskAccount, maskChannelName } from '@/lib/mask'
import { cn } from '@/lib/utils'

const SUBSCRIPTION_PROVIDERS = new Set(['anthropic_max', 'codex'])

function hostOf(base: string): string {
  try {
    return new URL(base).host
  } catch {
    return base
  }
}

export interface ChannelGridCardProps {
  channel: ChannelRow
  enabled: boolean
  orphan: boolean
  selected: boolean
  rowHealth: 'normal' | 'warning' | 'destructive' | 'disabled'
  rowStat: ChannelStatRow | undefined
  hourlyStart?: number
  rowUsage: ChannelUsageRow | undefined
  usageSettled: boolean
  egress?: { label: string; title: string; mono: boolean }
  testing: boolean
  balancing: boolean
  pinning: boolean
  statusPending: boolean
  onTogglePick: () => void
  onToggleStatus: (on: boolean) => void
  onTogglePin: () => void
  onTest: () => void
  onBalance: () => void
  onEdit: (focus?: ChannelEditFocus) => void
  onTry: () => void
  onDuplicate: () => void
  onDelete: () => void
}

/// 横向网格视图下的渠道卡片：顶部身份与控制 / 中部 24h 绿色成功率柱 / 底部额度余额与 30 天用量。
export function ChannelGridCard({
  channel: c,
  enabled,
  orphan,
  selected,
  rowHealth,
  rowStat,
  hourlyStart,
  rowUsage,
  usageSettled,
  egress,
  testing,
  balancing,
  pinning,
  statusPending,
  onTogglePick,
  onToggleStatus,
  onTogglePin,
  onTest,
  onBalance,
  onEdit,
  onTry,
  onDuplicate,
  onDelete,
}: ChannelGridCardProps) {
  const { t } = useTranslation()
  const displayName = maskChannelName(c.name, c.keys.map((k) => k.account_label))
  const plans = [...new Set(c.keys.map((k) => k.account_plan).filter((v): v is string => Boolean(v)))]
  const accounts = [...new Set(c.keys.map((k) => k.account_label).filter((v): v is string => Boolean(v)))]
  const extra = accounts.filter((label) => label !== c.name)
  const consoleUrl = providerConsoleUrl(c.provider, c.api_base)
  const host = !SUBSCRIPTION_PROVIDERS.has(c.provider) && c.api_base ? hostOf(c.api_base) : null
  const pools = c.pools ?? []
  const hasKeyAlert = enabled && c.keys.length > 0 && c.keys.some((k) => k.status !== 1)
  const canQueryBalance = enabled && balanceSupported(c.provider)
  const pinned = Boolean(c.pinned)

  return (
    <article
      data-slot="channel-card"
      data-layout="card"
      data-selected={selected || undefined}
      data-enabled={enabled || undefined}
      data-pinned={pinned || undefined}
      data-health={rowHealth}
      className={cn(
        'group/row relative flex h-full min-w-0 flex-col justify-between overflow-hidden rounded-2xl border bg-card shadow-card transition-[border-color,box-shadow,background-color] duration-150',
        selected
          ? 'border-primary/55 bg-primary/[0.025] ring-1 ring-primary/25 dark:bg-primary/[0.06]'
          : rowHealth === 'destructive'
            ? 'border-destructive/30 hover:border-destructive/50'
            : rowHealth === 'warning'
              ? 'border-warning/35 hover:border-warning/55'
              : pinned
                ? 'border-primary/30 hover:border-primary/50 hover:shadow-[0_4px_14px_-4px_oklch(0.24_0.015_70/0.09),0_1px_2px_oklch(0.24_0.015_70/0.04)] dark:hover:shadow-[inset_0_1px_0_oklch(1_0_0/0.07),0_8px_22px_-6px_oklch(0_0_0/0.55)]'
                : enabled
                  ? 'border-border/80 hover:border-border hover:shadow-[0_4px_14px_-4px_oklch(0.24_0.015_70/0.09),0_1px_2px_oklch(0.24_0.015_70/0.04)] dark:hover:shadow-[inset_0_1px_0_oklch(1_0_0/0.07),0_8px_22px_-6px_oklch(0_0_0/0.55)]'
                  : 'border-border/65 bg-card/70 hover:border-border/90 hover:bg-card/90',
      )}
    >
      {/* 左侧 3px 圆角语义状态条 */}
      <span
        aria-hidden
        className={cn(
          'pointer-events-none absolute top-3 bottom-3 left-[3px] w-[3px] rounded-full transition-colors',
          selected
            ? 'bg-primary'
            : rowHealth === 'destructive'
              ? 'bg-destructive'
              : rowHealth === 'warning'
                ? 'bg-warning'
                : pinned
                  ? 'bg-primary/55'
                  : enabled
                    ? 'bg-transparent group-hover/row:bg-primary/45'
                    : 'bg-transparent',
        )}
      />

      {/* 顶部：身份与控制（flex-1 让同行卡片的 24h 柱状带与底部遥测带水平对齐） */}
      <div className="flex-1 space-y-2.5 px-4 pt-3.5 pb-3">
        <div className="flex items-start justify-between gap-2">
          <div className="flex min-w-0 flex-1 items-center gap-2.5">
            <Checkbox
              srLabel={t('admin:batchPick', { name: displayName })}
              checked={selected}
              onChange={onTogglePick}
            />
            <ProviderMark
              provider={c.provider}
              size="sm"
              className={cn(enabled ? 'group-hover/row:scale-[1.03]' : 'opacity-45 grayscale')}
            />
            <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-1.5 gap-y-0.5">
              <button
                type="button"
                data-slot="channel-name"
                onClick={() => onEdit('conn')}
                className={cn(
                  'max-w-full cursor-pointer truncate rounded-sm text-left text-[14.5px] leading-5 font-semibold tracking-[-0.012em] underline-offset-2 outline-none transition-colors hover:text-primary hover:underline focus-visible:ring-2 focus-visible:ring-primary/40',
                  !enabled && 'text-muted-foreground',
                )}
                title={displayName}
              >
                {displayName}
              </button>
              <span className="shrink-0 font-mono text-[11px] text-muted-foreground/55 tabular-nums">
                #{c.id}
              </span>
              {pinned && (
                <span
                  data-slot="channel-pinned-badge"
                  className="inline-flex h-[18px] shrink-0 items-center gap-0.5 rounded bg-primary/10 px-1.5 text-[10.5px] font-medium text-primary ring-1 ring-inset ring-primary/20"
                >
                  <Pin aria-hidden className="h-2.5 w-2.5 fill-current" />
                  {t('admin:channelPinned')}
                </span>
              )}
              {consoleUrl !== null && (
                <a
                  href={consoleUrl}
                  target="_blank"
                  rel="noreferrer noopener"
                  className="-my-1 shrink-0 rounded-md p-1 text-muted-foreground opacity-0 transition-opacity group-hover/row:opacity-100 hover:bg-muted hover:text-foreground focus-visible:opacity-100 [@media(hover:none)]:opacity-100"
                  title={t('admin:providerConsole')}
                  aria-label={t('admin:providerConsole')}
                >
                  <ExternalLink className="h-3.5 w-3.5" />
                </a>
              )}
            </div>
          </div>

          <div className="flex shrink-0 items-center gap-2">
            <Switch
              hideLabel
              className="[&>span:last-child]:mt-0"
              label={t('admin:channelScheduleToggle', { name: displayName })}
              checked={enabled}
              disabled={statusPending}
              onChange={onToggleStatus}
            />
            <ActionGroup label={t('common:actions')}>
              <IconButton
                icon={Pin}
                label={pinned ? t('admin:channelUnpin') : t('admin:channelPin')}
                pressed={pinned}
                loading={pinning}
                onClick={onTogglePin}
                className={cn(
                  pinned && 'bg-primary/10 text-primary hover:bg-primary/15 hover:text-primary [&_svg]:fill-current',
                )}
              />
              <IconButton
                icon={Activity}
                label={t('admin:testChannel')}
                loading={testing}
                onClick={onTest}
              />
              <IconButton
                icon={Pencil}
                label={t('common:edit')}
                onClick={() => onEdit()}
              />
              <Menu
                strategy="fixed"
                className="min-w-44"
                trigger={
                  <Button
                    size="icon"
                    variant="ghost"
                    aria-label={t('admin:channelMoreActions')}
                    className={ICON_BUTTON_CLASS}
                  >
                    <Ellipsis />
                  </Button>
                }
              >
                <MenuItem icon={MessageSquareText} onSelect={onTry}>
                  {t('admin:channelTryAction')}
                </MenuItem>
                {balanceSupported(c.provider) && (
                  <MenuItem icon={Wallet} disabled={balancing} onSelect={onBalance}>
                    {t('admin:balanceQuery')}
                  </MenuItem>
                )}
                <MenuItem icon={Gauge} onSelect={() => onEdit('limits')}>
                  {t('admin:channelLimitsTitle')}
                </MenuItem>
                <MenuItem icon={Copy} onSelect={onDuplicate}>
                  {t('admin:duplicate')}
                </MenuItem>
                <MenuSeparator />
                <MenuItem icon={Trash2} destructive onSelect={onDelete}>
                  {t('common:delete')}
                </MenuItem>
              </Menu>
            </ActionGroup>
          </div>
        </div>

        {/* 状态与测活行 */}
        <div data-slot="channel-status" className="flex flex-wrap items-center gap-x-2 gap-y-1.5">
          <ProviderChip provider={c.provider} plans={plans} compact className={cn(!enabled && 'opacity-75')} />
          <KeyStateSummary
            keys={c.keys}
            enabled={enabled}
            onInspectKeys={() => onEdit(c.keys.length === 0 ? 'conn' : 'keys')}
          />
          {orphan && (
            <Tooltip
              content={
                <div className="w-[240px] max-w-full space-y-2 text-left">
                  <div className="flex items-start gap-2 text-[11.5px] leading-relaxed text-foreground">
                    <span aria-hidden className="mt-1.5 size-1.5 shrink-0 rounded-full bg-destructive" />
                    <span>{t('admin:poolOrphanWarning')}</span>
                  </div>
                  <div className="border-t border-border/60 pt-1.5 text-[11px] text-muted-foreground">
                    {t('admin:channelClickPools')}
                  </div>
                </div>
              }
            >
              <button
                type="button"
                onClick={() => onEdit('pools')}
                className="inline-flex min-h-[22px] shrink-0 cursor-pointer items-center rounded-md bg-destructive/10 px-2 py-0 text-[11px] font-medium whitespace-nowrap text-destructive ring-1 ring-inset ring-destructive/22 outline-none transition-colors hover:bg-destructive/16 focus-visible:ring-2 focus-visible:ring-primary/40"
              >
                {t('admin:channelOrphan')}
              </button>
            </Tooltip>
          )}
          <div className="ml-auto">
            <LastProbe probe={c.last_test} probing={testing} onProbe={onTest} />
          </div>
        </div>

        {/* 紧凑路由摘要行 */}
        <div
          data-slot="channel-meta"
          className={cn(
            'flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[11.5px] leading-5 text-muted-foreground',
            !enabled && 'opacity-80',
          )}
        >
          {pools.length > 0 && (
            <button
              type="button"
              onClick={() => onEdit('pools')}
              className="inline-flex h-[19px] max-w-[8.5rem] shrink-0 cursor-pointer items-center truncate rounded-md bg-muted px-1.5 font-mono text-[11px] font-medium text-foreground/85 ring-1 ring-inset ring-border/75 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 dark:bg-white/[0.08]"
              title={pools.join(', ')}
            >
              {pools.length <= 2 ? pools.join(', ') : `${pools[0]} +${pools.length - 1}`}
            </button>
          )}
          <button
            type="button"
            onClick={() => onEdit('models')}
            className="-mx-1 shrink-0 cursor-pointer rounded px-1 font-medium whitespace-nowrap text-foreground/80 outline-none transition-colors hover:bg-accent/80 hover:text-primary focus-visible:ring-2 focus-visible:ring-primary/40"
          >
            {t('admin:channelModelCount', { n: c.models.length })}
          </button>
          <button
            type="button"
            onClick={() => onEdit('schedule')}
            className="-mx-1 inline-flex shrink-0 cursor-pointer items-center gap-1 rounded px-1 whitespace-nowrap tabular-nums outline-none transition-colors hover:bg-accent/80 hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40"
          >
            <span>
              {t('admin:priority')}{' '}
              <span className={cn(c.priority > 0 ? 'font-semibold text-foreground' : 'font-medium text-foreground/75')}>
                {c.priority}
              </span>
            </span>
            {c.cost_milli !== 1000 && (
              <span className="rounded bg-primary/10 px-1 py-px font-mono text-[10.5px] font-semibold text-primary ring-1 ring-inset ring-primary/20">
                ×{costMilliToRatio(c.cost_milli)}
              </span>
            )}
          </button>
          {c.keys.length > 1 && (
            <button
              type="button"
              onClick={() => onEdit('keys')}
              className="inline-flex h-[19px] shrink-0 cursor-pointer items-center gap-1 rounded-md bg-muted/75 px-1.5 text-[11px] font-medium text-foreground/80 tabular-nums ring-1 ring-inset ring-border/60 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 dark:bg-white/[0.06]"
            >
              <KeyRound aria-hidden className="h-3 w-3 opacity-75" />
              {c.keys.length}
            </button>
          )}
          {extra.length > 0 ? (
            <span
              data-slot="channel-account"
              className="max-w-[11rem] truncate text-foreground/80"
              title={accounts.map(maskAccount).join('\n')}
            >
              {maskAccount(extra[0])}
            </span>
          ) : (
            host !== null && (
              <span
                data-slot="channel-host"
                className="max-w-[11rem] truncate font-mono text-[11px] text-muted-foreground/85"
                title={c.api_base ?? undefined}
              >
                {host}
              </span>
            )
          )}
          {egress !== undefined && (
            <button
              type="button"
              data-slot="channel-egress"
              onClick={() => onEdit('egress')}
              className="-mx-1 inline-flex min-w-0 cursor-pointer items-center gap-1 rounded px-1 outline-none transition-colors hover:bg-accent/80 hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40"
              aria-label={`${t('admin:channelStatusEgress')}: ${egress.label} · ${t('admin:channelClickEgress')}`}
            >
              <Globe aria-hidden className="h-3 w-3 shrink-0 text-muted-foreground/70" strokeWidth={1.8} />
              <span className={cn('max-w-[8.5rem] truncate text-[11px]', egress.mono && 'tabular-nums')} title={egress.title}>
                {egress.label}
              </span>
            </button>
          )}
        </div>

        {hasKeyAlert && (
          <KeyStateDetail keys={c.keys} enabled={enabled} onInspect={() => onEdit('keys')} />
        )}
      </div>

      {/* 中部核心：24 小时成功率绿色柱状条 */}
      <div data-slot="channel-health" className="border-t border-border/55 px-4 py-3">
        <Health24h
          stat={rowStat}
          hourlyStart={hourlyStart}
          channel={{ id: c.id, name: c.name, provider: c.provider }}
          layout="card"
        />
      </div>

      {/* 底部遥测带：左额度/余额，右 30 天用量 */}
      <div className="border-t border-border/60 bg-muted/[0.26] px-4 py-2.5 dark:bg-white/[0.022]">
        <div className="grid grid-cols-[minmax(0,1.45fr)_minmax(0,0.9fr)] items-center gap-2.5">
          <div data-slot="channel-quota" className="flex min-w-0 flex-col justify-center gap-1.5">
            <div
              title={enabled ? t('admin:channelClickLimits') : undefined}
              onClick={(e) => {
                if (!enabled) return
                if ((e.target as HTMLElement).closest('button, a')) return
                onEdit('limits')
              }}
              className={cn(
                'min-w-0 empty:hidden',
                enabled && '-mx-1.5 -my-0.5 cursor-pointer rounded-lg px-1.5 py-0.5 transition-colors hover:bg-accent/65',
              )}
            >
              <ChannelQuotaCell
                channelId={c.id}
                provider={c.provider}
                control={c.settings?.account_control}
                enabled={enabled}
                fallback={
                  c.last_balance ? null : canQueryBalance ? (
                    <div className="flex flex-col gap-0.5">
                      <span className="text-[11px] leading-4 font-medium text-muted-foreground/80">
                        {t('admin:channelColQuota')}
                      </span>
                      <button
                        type="button"
                        disabled={balancing}
                        onClick={onBalance}
                        className="-ml-1.5 inline-flex w-fit cursor-pointer items-center gap-1 rounded-md px-1.5 py-0.5 text-xs font-medium text-primary outline-none transition-colors hover:bg-primary/10 focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-wait disabled:opacity-60"
                      >
                        {balancing ? (
                          <Loader2 aria-hidden className="h-3 w-3 animate-spin" />
                        ) : (
                          <Wallet aria-hidden className="h-3 w-3" />
                        )}
                        {t('admin:balanceQuery')}
                      </button>
                    </div>
                  ) : (
                    <div className="flex flex-col gap-0.5">
                      <span className="text-[11px] leading-4 font-medium text-muted-foreground/75">
                        {t('admin:channelColQuota')}
                      </span>
                      <span className="text-sm leading-5 text-muted-foreground/60" aria-hidden>—</span>
                    </div>
                  )
                }
              />
            </div>
            <LastBalance
              balance={c.last_balance}
              refreshing={balancing}
              onRefresh={balanceSupported(c.provider) ? onBalance : undefined}
            />
          </div>
          <div className="flex min-w-0 flex-col justify-center">
            <ChannelUsage usage={rowUsage} channelId={c.id} settled={usageSettled} />
          </div>
        </div>
      </div>
    </article>
  )
}
