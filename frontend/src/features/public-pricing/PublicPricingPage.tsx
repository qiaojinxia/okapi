import { Link } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, Moon, Sun, Tags } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { PricingModel } from '@/features/public-pricing/types'
import { BrandLockup } from '@/components/brand'
import { Badge } from '@/components/ui/badge'
import { Button, buttonVariants } from '@/components/ui/button'
import { Label } from '@/components/ui/input'
import { SearchInput } from '@/components/ui/search-input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Simulator } from '@/features/public-pricing/Simulator'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch, getKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney, formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { useTheme } from '@/lib/theme'

type TokenUnit = '1K' | '1M'

interface PricingGroup {
  code: string
  name: string | null
  ratio: string | null
}

/// 倍率 → 每单位 tokens 美元价（基准 $2/1M per 倍率 1.0，DESIGN §3；与 new-api
/// `model_ratio × 2 × groupRatio` 同口径）。
function unitPriceMicro(
  ratio: string | null,
  factor = 1,
  unit: TokenUnit = '1M',
): number | null {
  if (ratio === null) return null
  const r = Number(ratio)
  if (Number.isNaN(r)) return null
  const perMillion = r * factor * 2_000_000
  return unit === '1M' ? perMillion : perMillion / 1000
}

/// 公开价格页（免登录）。
///
/// 这页是中转站的"橱窗"（DESIGN §9.4），也是聚合比价工具收录的入口：
/// 顶部给品牌与登录入口，表格带搜索与粘性表头——模型上百时不搜就找不到，
/// 滚到第 80 行看不见表头就不知道哪列是缓存读取。
export function PublicPricingPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const theme = useTheme()
  const [unit, setUnit] = useState<TokenUnit>('1M')
  const [group, setGroup] = useState('')
  const [search, setSearch] = useState('')
  const pricing = useQuery({
    queryKey: qk.publicPricing,
    queryFn: () =>
      apiFetch<{ models: PricingModel[]; groups: PricingGroup[] }>('/api/pricing'),
  })
  const groups = pricing.data?.groups ?? []
  // 选中分组的倍率一并折进展示价：用户看到的就是自己那档的实价
  const groupFactor = Number(groups.find((g) => g.code === group)?.ratio ?? '1') || 1
  const kw = search.trim().toLowerCase()
  const models = (pricing.data?.models ?? []).filter(
    (m) =>
      kw === '' ||
      m.model.toLowerCase().includes(kw) ||
      (m.vendor ?? '').toLowerCase().includes(kw) ||
      (m.display_name ?? '').toLowerCase().includes(kw),
  )
  const signedIn = getKey() !== null

  return (
    <div className="min-h-screen bg-background">
      <header className="sticky top-0 z-20 border-b border-border bg-background/85 backdrop-blur-md">
        <div className="mx-auto flex h-14 w-full max-w-6xl items-center justify-between gap-3 px-4 sm:px-6">
          <Link to="/" className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
            <BrandLockup />
          </Link>
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="icon"
              className="h-9 w-9"
              aria-label={t('common:theme')}
              onClick={theme.toggle}
            >
              {theme.resolved === 'dark' ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
            </Button>
            <Link to={signedIn ? '/portal' : '/'} className={buttonVariants({ size: 'sm' })}>
              {signedIn ? t('common:portal') : t('common:login')}
              <ArrowRight className="h-3.5 w-3.5" />
            </Link>
          </div>
        </div>
      </header>

      <main className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-4 py-8 sm:px-6">
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-3">
            <span className="flex h-10 w-10 items-center justify-center rounded-lg bg-primary/10 text-primary">
              <Tags className="h-5 w-5" />
            </span>
            <h1 className="text-2xl font-semibold tracking-tight">{t('pricing:title')}</h1>
          </div>
          <p className="max-w-3xl text-sm leading-6 text-muted-foreground">{t('pricing:baseNote')}</p>
        </div>

        <div className="flex flex-wrap items-end justify-between gap-3 rounded-lg border border-border bg-card p-3 shadow-card">
          <div className="flex flex-wrap items-end gap-3">
            <SearchInput
              className="w-64"
              value={search}
              placeholder={t('pricing:searchHint')}
              onChange={setSearch}
            />
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="group">{t('pricing:viewAsGroup')}</Label>
              <Select
                id="group"
                className="w-60"
                value={group}
                onChange={setGroup}
                placeholder={t('pricing:baseGroup')}
                options={groups.map((g) => ({
                  value: g.code,
                  label: `${g.name ?? g.code} ×${formatRatio(g.ratio ?? '1')}`,
                }))}
              />
            </div>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label>{t('pricing:tokenUnit')}</Label>
            <Segmented
              ariaLabel={t('pricing:tokenUnit')}
              value={unit}
              onChange={setUnit}
              options={[
                { value: '1M', label: '1M tokens' },
                { value: '1K', label: '1K tokens' },
              ]}
            />
          </div>
        </div>

        {pricing.isError ? (
          <ErrorState message={describeError(pricing.error)} onRetry={() => void pricing.refetch()} />
        ) : pricing.isPending ? (
          <TableSkeleton rows={10} cols={9} />
        ) : (
          <>
            {models.length === 0 ? (
              <EmptyState title={t('common:noResults')} hint={t('common:noResultsHint')} />
            ) : (
              <Table stickyHeader wrapperClassName="max-h-[70vh]">
                <THead>
                  <Tr>
                    <Th>{t('pricing:model')}</Th>
                    <Th numeric>{t('pricing:ratio')}</Th>
                    <Th numeric>{t('pricing:promptPrice')}</Th>
                    <Th numeric>{t('pricing:completionPrice')}</Th>
                    <Th numeric>{t('pricing:cachedPrice')}</Th>
                    <Th numeric>{t('pricing:cacheWritePrice')}</Th>
                    <Th numeric>{t('pricing:audioInPrice')}</Th>
                    <Th numeric>{t('pricing:audioOutPrice')}</Th>
                    <Th numeric>{t('pricing:imageInPrice')}</Th>
                  </Tr>
                </THead>
                <TBody>
                  {models.map((m) => {
                    // 分组视角下打不到的模型置灰而非隐藏：让用户知道"存在但我这组不可用"，
                    // 这正是升级分组的动机；全量视角(未选组)不置灰
                    const usable = group === '' || m.groups.includes(group)
                    const prompt = unitPriceMicro(m.model_ratio, groupFactor, unit)
                    const completion = unitPriceMicro(
                      m.model_ratio,
                      groupFactor * Number(m.completion_ratio ?? '1'),
                      unit,
                    )
                    const cached = unitPriceMicro(
                      m.model_ratio,
                      groupFactor * Number(m.cache_ratio ?? '1'),
                      unit,
                    )
                    // 写入倍率为 1 时该模型不区分缓存写入（OpenAI 隐式缓存），显示 —
                    const cacheWriteRatio = Number(m.cache_write_ratio ?? '1')
                    const cacheWrite =
                      cacheWriteRatio === 1
                        ? null
                        : unitPriceMicro(m.model_ratio, groupFactor * cacheWriteRatio, unit)
                    // 模态倍率为 1 表示该模型不区分模态（纯文本模型），显示 — 而非重复文本价
                    const audioRatio = Number(m.audio_ratio ?? '1')
                    const audioIn =
                      audioRatio === 1
                        ? null
                        : unitPriceMicro(m.model_ratio, groupFactor * audioRatio, unit)
                    const audioOut =
                      audioRatio === 1
                        ? null
                        : unitPriceMicro(
                            m.model_ratio,
                            groupFactor * audioRatio * Number(m.audio_completion_ratio ?? '1'),
                            unit,
                          )
                    const imageRatio = Number(m.image_ratio ?? '1')
                    const imageIn =
                      imageRatio === 1
                        ? null
                        : unitPriceMicro(m.model_ratio, groupFactor * imageRatio, unit)
                    const price = (v: number | null) =>
                      v === null ? (
                        <span className="text-muted-foreground/60">—</span>
                      ) : (
                        formatMoney(v, locale)
                      )
                    return (
                      <Tr key={m.model} className={usable ? undefined : 'opacity-45'}>
                        <Td className="min-w-64">
                          <div className="flex items-center gap-2">
                            <span className="font-mono text-xs font-medium">{m.model}</span>
                            {m.vendor && <Badge variant="outline">{m.vendor}</Badge>}
                            {!usable && (
                              <Badge variant="warning">{t('pricing:notInGroup')}</Badge>
                            )}
                          </div>
                          {/* 可用分组徽章：用户在价格页最想知道的就是"用哪个组能打到它"。
                              封顶 4 个 + "+N"：分组多的站点不能让每行长成一列标签 */}
                          {m.groups.length > 0 && (
                            <div className="mt-1 flex flex-wrap gap-1">
                              {m.groups.slice(0, 4).map((code) => {
                                const info = groups.find((g) => g.code === code)
                                return (
                                  <Badge key={code} variant="muted" className="font-mono text-[10px]">
                                    {code} ×{formatRatio(info?.ratio ?? '1')}
                                  </Badge>
                                )
                              })}
                              {m.groups.length > 4 && (
                                <Badge
                                  variant="muted"
                                  className="text-[10px]"
                                  title={m.groups.slice(4).join(', ')}
                                >
                                  +{m.groups.length - 4}
                                </Badge>
                              )}
                            </div>
                          )}
                        </Td>
                        {m.mode === 'per_call' ? (
                          <Td colSpan={8} className="text-muted-foreground">
                            {t('pricing:perCall', {
                              price: formatMoney(m.per_call_price_micro ?? 0, locale),
                            })}
                          </Td>
                        ) : (
                          <>
                            <Td numeric>{formatRatio(m.model_ratio)}</Td>
                            <Td numeric className="font-medium">
                              {price(prompt)}
                            </Td>
                            <Td numeric className="font-medium">
                              {price(completion)}
                            </Td>
                            <Td numeric>{price(cached)}</Td>
                            <Td numeric>{price(cacheWrite)}</Td>
                            <Td numeric>{price(audioIn)}</Td>
                            <Td numeric>{price(audioOut)}</Td>
                            <Td numeric>{price(imageIn)}</Td>
                          </>
                        )}
                      </Tr>
                    )
                  })}
                </TBody>
              </Table>
            )}

            <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,2fr)]">
              <div className="flex flex-col gap-3 rounded-lg border border-border bg-card p-5 shadow-card">
                <h3 className="text-sm font-semibold">{t('pricing:groups')}</h3>
                <p className="text-xs leading-5 text-muted-foreground">{t('pricing:groupsHint')}</p>
                <div className="flex flex-wrap gap-2">
                  {groups.map((g) => (
                    <Badge key={g.code} variant={g.code === group ? 'default' : 'muted'}>
                      {g.name ?? g.code} ×{formatRatio(g.ratio ?? '1')}
                    </Badge>
                  ))}
                </div>
              </div>
              <Simulator models={pricing.data?.models ?? []} />
            </div>
          </>
        )}
      </main>

      <footer className="border-t border-border">
        <div className="mx-auto flex w-full max-w-6xl flex-wrap items-center justify-between gap-2 px-4 py-6 text-xs text-muted-foreground sm:px-6">
          <span>{t('common:appName')}</span>
          <Link to="/" className="underline decoration-dotted underline-offset-4 hover:text-foreground">
            {t('common:login')}
          </Link>
        </div>
      </footer>
    </div>
  )
}
