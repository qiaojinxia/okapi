import { useTranslation } from 'react-i18next'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { presetEndpoints, useCompatVendors } from '@/features/channels/compat'

/// 新建兼容渠道时的「厂商预设」：选了 DeepSeek / Moonshot / 适配档案，就带上它的接入地址与特殊要求
/// （渠道的厂商字段随创建一起落库）。兼容渠道没有缺省地址——不选厂商就必须自己填。
///
/// 选厂商会把它的端点填进接入地址，但只在地址还是空的、或还是上一个预设填的值时才覆盖：
/// 管理员手填的地址（中转站、自建网关）不会被悄悄换掉。有多个接入点（国内 / International）时再给一个选择。
export function VendorPreset({
  vendor,
  apiBase,
  onPick,
}: {
  /// 当前选定：`builtin:xxx` / `custom:xxx`；没选为 null / undefined。
  vendor: string | null | undefined
  apiBase: string
  /// `base`：要写进接入地址的新值；`undefined` 表示不动地址。
  onPick: (vendor: string | null, base: string | undefined) => void
}) {
  const { t } = useTranslation()
  const vendors = useCompatVendors(true)
  const catalog = vendors.data
  const options = [
    ...(catalog?.data ?? []).map((v) => ({ value: v.id, label: v.display_name })),
    ...(catalog?.profiles ?? []).map((p) => ({ value: p.id, label: t('compat:profileOption', { name: p.display_name }) })),
  ]
  const endpoints = presetEndpoints(catalog, vendor)
  const typed = apiBase.trim()
  // 当前地址是不是「上一个预设填的」：此时换预设可以跟着换
  const owned = typed === '' || endpoints.some((e) => e.base === typed)

  const pick = (next: string) => {
    if (next === '') {
      onPick(null, owned && typed !== '' ? '' : undefined)
      return
    }
    const first = presetEndpoints(catalog, next)[0]?.base
    onPick(next, first !== undefined && owned ? first : undefined)
  }

  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor="d-preset">{t('compat:channelPreset')}</Label>
      <p className="text-xs text-muted-foreground">{t('compat:channelPresetHint')}</p>
      <Select
        id="d-preset"
        className="w-72"
        value={vendor ?? ''}
        placeholder={t('compat:channelPresetNone')}
        options={options}
        onChange={pick}
      />
      {endpoints.length > 1 && (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="d-preset-endpoint">{t('compat:channelPresetEndpoint')}</Label>
          <Select
            id="d-preset-endpoint"
            className="w-full max-w-md"
            value={endpoints.some((e) => e.base === typed) ? typed : ''}
            placeholder={t('compat:channelPresetEndpointKeep')}
            options={endpoints.map((e) => ({ value: e.base, label: e.label === undefined ? e.base : `${e.label} · ${e.base}` }))}
            onChange={(base) => base !== '' && onPick(vendor ?? null, base)}
          />
        </div>
      )}
    </div>
  )
}
