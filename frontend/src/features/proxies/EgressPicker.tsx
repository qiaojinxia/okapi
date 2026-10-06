import { useQuery } from '@tanstack/react-query'
import type { TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'
import { Select } from '@/components/ui/select'
import { describeError } from '@/lib/i18n'
import { egressDefaultOptions, proxyGroupOptions, proxyOptions } from './options'
import type { EgressBinding, EgressMode, ProxyGroupRow, ProxyRow } from './types'
import { GROUP_MODE_LABEL } from './types'

/// 编辑中的出口（目标可能还没选）；`toBinding` 补全后才能提交。
export interface EgressDraft {
  mode: EgressMode
  proxy_id?: number
  group_code?: string
}

export function draftOf(binding: EgressBinding | undefined | null): EgressDraft {
  switch (binding?.mode) {
    case 'direct':
      return { mode: 'direct' }
    case 'proxy':
      return { mode: 'proxy', proxy_id: binding.proxy_id }
    case 'group':
      return { mode: 'group', group_code: binding.group_code }
    default:
      return { mode: 'inherit' }
  }
}

/// 目标没选全 → null（调用方据此禁用提交）。
export function toBinding(draft: EgressDraft): EgressBinding | null {
  switch (draft.mode) {
    case 'inherit':
    case 'direct':
      return { mode: draft.mode }
    case 'proxy':
      return draft.proxy_id === undefined ? null : { mode: 'proxy', proxy_id: draft.proxy_id }
    case 'group':
      return draft.group_code === undefined || draft.group_code === ''
        ? null
        : { mode: 'group', group_code: draft.group_code }
  }
}

export function sameBinding(a: EgressBinding | null, b: EgressBinding | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

/// 一句话描述一个出口（列表徽章、摘要用）。
export function describeEgress(
  t: TFunction,
  binding: EgressBinding | undefined | null,
  proxies: ProxyRow[] | undefined,
  groups: ProxyGroupRow[] | undefined,
): string {
  switch (binding?.mode) {
    case 'direct':
      return t('admin:egressDirect')
    case 'proxy': {
      const proxy = proxies?.find((p) => p.id === binding.proxy_id)
      return t('admin:egressProxyNamed', { name: proxy?.name ?? `#${binding.proxy_id}` })
    }
    case 'group': {
      const group = groups?.find((g) => g.code === binding.group_code)
      return t('admin:egressGroupNamed', {
        name: group?.name ?? binding.group_code,
        mode: group ? t(GROUP_MODE_LABEL[group.mode]) : '',
      })
    }
    default:
      return t('admin:egressInherit')
  }
}

/// 出口选择器（受控）：继承全局默认 / 直连 / 单个代理 / 代理组。
///
/// 只列操作者看得见的代理与组（own 范围只见自己的，后端同样校验）。停用的代理仍可选——
/// 绑定是长期配置，停用多是临时维护；选中时就地提示「当前停用，绑定后该渠道不可调度」。
export function EgressPicker({
  value,
  onChange,
  allowInherit = true,
  idPrefix = 'egress',
}: {
  value: EgressDraft
  onChange: (next: EgressDraft) => void
  allowInherit?: boolean
  idPrefix?: string
}) {
  const { t } = useTranslation()
  const proxies = useQuery(proxyOptions())
  const groups = useQuery(proxyGroupOptions())
  const fallback = useQuery({ ...egressDefaultOptions(), enabled: allowInherit })
  const proxyRows = proxies.data ?? []
  const groupRows = groups.data ?? []
  const modes: { value: EgressMode; label: string }[] = [
    ...(allowInherit
      ? [{
          value: 'inherit' as const,
          label: t('admin:egressInheritWith', {
            current: describeEgress(t, fallback.data?.egress, proxyRows, groupRows),
          }),
        }]
      : []),
    { value: 'direct', label: t('admin:egressDirect') },
    { value: 'proxy', label: t('admin:egressModeProxy') },
    { value: 'group', label: t('admin:egressModeGroup') },
  ]
  const selectedProxy = proxyRows.find((p) => p.id === value.proxy_id)
  const selectedGroup = groupRows.find((g) => g.code === value.group_code)
  const loadError = proxies.error ?? groups.error

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <Select
          id={`${idPrefix}-mode`}
          aria-label={t('admin:egressMode')}
          className="w-56"
          value={value.mode}
          onChange={(mode) => onChange({ mode: mode as EgressMode })}
          options={modes}
        />
        {value.mode === 'proxy' && (
          <Select
            id={`${idPrefix}-proxy`}
            aria-label={t('admin:egressModeProxy')}
            className="min-w-56 flex-1"
            value={value.proxy_id === undefined ? '' : String(value.proxy_id)}
            placeholder={t('admin:egressPickProxy')}
            onChange={(id) => onChange({ mode: 'proxy', proxy_id: id === '' ? undefined : Number(id) })}
            options={proxyRows.map((p) => ({
              value: String(p.id),
              label: `${p.name} · ${p.url_masked}${p.status !== 1 ? ` (${t('common:disabled')})` : ''}`,
            }))}
          />
        )}
        {value.mode === 'group' && (
          <Select
            id={`${idPrefix}-group`}
            aria-label={t('admin:egressModeGroup')}
            className="min-w-56 flex-1"
            value={value.group_code ?? ''}
            placeholder={t('admin:egressPickGroup')}
            onChange={(code) => onChange({ mode: 'group', group_code: code === '' ? undefined : code })}
            options={groupRows.map((g) => ({
              value: g.code,
              label: `${g.name} · ${t(GROUP_MODE_LABEL[g.mode])} · ${t('admin:proxyGroupMemberCount', { n: g.members.length })}`,
            }))}
          />
        )}
      </div>
      {loadError && <p role="alert" className="text-xs text-destructive">{describeError(loadError)}</p>}
      {value.mode === 'proxy' && proxyRows.length === 0 && proxies.isSuccess && (
        <p className="text-xs text-muted-foreground">{t('admin:egressNoProxies')}</p>
      )}
      {value.mode === 'group' && groupRows.length === 0 && groups.isSuccess && (
        <p className="text-xs text-muted-foreground">{t('admin:egressNoGroups')}</p>
      )}
      {selectedProxy && selectedProxy.status !== 1 && (
        <p role="note" className="text-xs text-warning">{t('admin:egressProxyDisabledWarn')}</p>
      )}
      {selectedGroup && selectedGroup.members.length === 0 && (
        <p role="note" className="text-xs text-warning">{t('admin:egressGroupEmptyWarn')}</p>
      )}
      <p className="text-xs text-muted-foreground">
        {t(
          value.mode === 'inherit'
            ? 'admin:egressInheritHint'
            : value.mode === 'direct'
              ? 'admin:egressDirectHint'
              : value.mode === 'proxy'
                ? 'admin:egressProxyHint'
                : selectedGroup?.mode === 'rotate'
                  ? 'admin:proxyGroupRotateHint'
                  : 'admin:proxyGroupPinnedHint',
        )}
      </p>
    </div>
  )
}
