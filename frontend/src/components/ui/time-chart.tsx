import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Area, CartesianGrid, ComposedChart, Bar, Line, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'
import { Download, Table2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Segmented } from '@/components/ui/segmented'
import { Table, THead, TBody, Tr, Th, Td } from '@/components/ui/table'
import { Pagination } from '@/components/ui/pagination'
import { clampOffset, DEFAULT_PAGE_SIZE, PAGE_SIZES } from '@/hooks/use-pagination'
import { downloadCsv } from '@/lib/csv'
import { cn } from '@/lib/utils'

export interface ChartSeries { key: string; label: string; color: string; axis?: 'right' }
export interface ChartPoint { bucket: string; [key: string]: string | number | null }

export function chartNumber(n: number, locale: string): string {
  return new Intl.NumberFormat(locale, { notation: Math.abs(n) >= 10_000 ? 'compact' : 'standard', maximumFractionDigits: 2 }).format(n)
}

export function TimeChart({ data, series, format, unit, label, stacked = false, line = false, percent = false, defaultType = 'area', compact = false, fill = false, controls = true, paginateTable = false, secondaryAxis }: {
  data: ChartPoint[]
  series: ChartSeries[]
  format: (value: number) => string
  unit: string
  label: string
  stacked?: boolean
  line?: boolean
  percent?: boolean
  defaultType?: 'area' | 'bar'
  compact?: boolean
  /// 桌面端让绘图区撑满父级剩余高度（父级需为 flex 列），避免同排卡片更高时图下方留白。
  fill?: boolean
  controls?: boolean
  paginateTable?: boolean
  secondaryAxis?: { unit: string; format: (value: number) => string }
}) {
  const { t, i18n } = useTranslation()
  const id = useId().replaceAll(':', '')
  const [type, setType] = useState<'area' | 'bar'>(defaultType)
  const [table, setTable] = useState(false)
  const [tableOffset, setTableOffset] = useState(0)
  const [tableLimit, setTableLimit] = useState(DEFAULT_PAGE_SIZE)
  const offset = clampOffset(tableOffset, tableLimit, data.length)
  const tableRows = paginateTable ? data.slice(offset, offset + tableLimit) : data
  const [hidden, setHidden] = useState<string[]>([])
  const visible = series.filter((s) => !hidden.includes(s.key))
  const seriesUnit = (s: ChartSeries) => s.axis === 'right' && secondaryAxis ? secondaryAxis.unit : unit
  const seriesFormat = (s: ChartSeries | undefined) => s?.axis === 'right' && secondaryAxis ? secondaryAxis.format : format
  const seriesLabel = (s: ChartSeries) => secondaryAxis && seriesUnit(s) !== s.label ? `${s.label} (${seriesUnit(s)})` : s.label
  const exportRows = () => downloadCsv('usage-chart', [t('charts:date'), ...series.map((s) => `${s.label} (${seriesUnit(s)})`)], data.map((d) => [d.bucket, ...series.map((s) => d[s.key])]))
  const legend = <div role="group" aria-label={t('charts:legend')} className={cn('flex flex-wrap gap-1.5', !secondaryAxis && 'border-t border-border/60', !secondaryAxis && (compact ? 'pt-1' : 'pt-3'))}>
    {series.map((s) => <button type="button" key={s.key} aria-pressed={!hidden.includes(s.key)} disabled={!hidden.includes(s.key) && visible.length === 1} onClick={() => setHidden((previous) => previous.includes(s.key) ? previous.filter((key) => key !== s.key) : [...previous, s.key])} className={cn('inline-flex min-h-8 max-w-full items-center gap-2 rounded-md px-2 text-xs outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-default', secondaryAxis && 'px-1', hidden.includes(s.key) && 'opacity-45')}>
      <span className={cn('shrink-0', s.axis === 'right' ? 'w-3 border-t-2 border-dashed' : 'h-2 w-2 rounded-full')} style={s.axis === 'right' ? { borderColor: s.color } : { background: s.color }} />
      <span className="truncate">{seriesLabel(s)}{secondaryAxis && <span className="ml-1 text-muted-foreground">{t(s.axis === 'right' ? 'charts:rightAxis' : 'charts:leftAxis')}</span>}</span>
    </button>)}
  </div>
  return (
    <div className={cn('min-w-0', compact ? 'space-y-2' : 'space-y-4', fill && 'flex flex-col lg:min-h-0 lg:flex-1')} role="group" aria-label={label}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        {secondaryAxis ? <><span className="sr-only">{t('charts:dualAxis', { left: unit, right: secondaryAxis.unit })}</span>{legend}</> : <span className="text-xs text-muted-foreground">{t('charts:unit', { unit })}</span>}
        {controls && <div className="flex flex-wrap items-center gap-2">
          {!line && !secondaryAxis && <Segmented size="sm" ariaLabel={t('charts:type')} value={type} onChange={setType} options={[{ value: 'area', label: t('charts:area') }, { value: 'bar', label: t('charts:bar') }]} />}
          <Button variant="ghost" size="sm" aria-pressed={table} onClick={() => setTable(!table)}><Table2 className="h-3.5 w-3.5" />{t('charts:table')}</Button>
          <Button variant="outline" size="sm" onClick={exportRows}><Download className="h-3.5 w-3.5" />{t('charts:export')}</Button>
        </div>}
      </div>
      {table ? (
        <div className="space-y-3">
        <Table dense stickyHeader wrapperClassName="max-h-80" className="tabular-nums" aria-label={label}>
          <THead><Tr><Th>{t('charts:date')}</Th>{series.map((s) => <Th key={s.key} numeric>{seriesLabel(s)}</Th>)}</Tr></THead>
          <TBody>{tableRows.map((point) => <Tr key={point.bucket}><Td className="whitespace-nowrap">{point.bucket}</Td>{series.map((s) => <Td key={s.key} numeric>{typeof point[s.key] === 'number' ? seriesFormat(s)(point[s.key] as number) : '—'}</Td>)}</Tr>)}</TBody>
        </Table>
        {paginateTable && <Pagination total={data.length} limit={tableLimit} offset={tableOffset} onOffset={setTableOffset} pageSizes={PAGE_SIZES} onLimit={(limit) => { setTableLimit(limit); setTableOffset(0) }} className="rounded-lg shadow-none" />}
        </div>
      ) : (
        <div className={cn('min-w-0', fill ? 'h-44 lg:relative lg:h-auto lg:min-h-36 lg:flex-1' : compact ? 'h-44 lg:h-[clamp(8rem,calc(100dvh-40rem),14rem)]' : 'h-72 sm:h-80')} aria-label={t('charts:plot')}>
          <ResponsiveContainer width="100%" height="100%" minWidth={0} className={fill ? 'lg:absolute lg:inset-0' : undefined}>
            <ComposedChart data={data} margin={{ top: 10, right: 10, bottom: 4, left: 0 }} accessibilityLayer>
              <defs>{series.map((s, i) => <linearGradient key={s.key} id={`${id}-${i}`} x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor={s.color} stopOpacity={0.28} /><stop offset="100%" stopColor={s.color} stopOpacity={0.025} /></linearGradient>)}</defs>
              <CartesianGrid vertical={false} stroke="var(--color-border)" strokeDasharray="3 5" strokeOpacity={0.7} />
              <XAxis dataKey="bucket" tickFormatter={(value: string) => value.length > 10 ? `${value.slice(5, 10)} ${value.slice(11, 16)}` : value.slice(5)} tick={{ fontSize: 11, fill: 'var(--color-muted-foreground)' }} axisLine={false} tickLine={false} minTickGap={32} tickMargin={10} />
              <YAxis width={54} domain={percent ? [0, 100] : [0, 'auto']} tick={{ fontSize: 11, fill: 'var(--color-muted-foreground)' }} tickFormatter={(n) => `${chartNumber(Number(n), i18n.language)}${percent ? '%' : ''}`} axisLine={false} tickLine={false} tickMargin={8} />
              {secondaryAxis && <YAxis yAxisId="right" orientation="right" width={46} domain={[0, 'auto']} tick={{ fontSize: 11, fill: 'var(--color-muted-foreground)' }} tickFormatter={(n) => chartNumber(Number(n), i18n.language)} axisLine={false} tickLine={false} tickMargin={8} />}
              <Tooltip cursor={{ stroke: 'var(--color-muted-foreground)', strokeDasharray: '4 4', fill: 'var(--color-muted)', fillOpacity: 0.25 }} content={({ active, payload, label: date }) => {
                if (!active || !payload?.length) return null
                const points = payload.filter((p) => typeof p.value === 'number')
                return <div className="max-w-[min(22rem,80vw)] rounded-xl border border-border bg-popover p-3 text-xs text-popover-foreground shadow-popover"><p className="mb-2 font-medium">{String(date)}</p><div className="max-h-56 space-y-2 overflow-auto">{points.map((p) => <div key={String(p.dataKey)} className="flex items-center gap-2"><span className="h-2 w-2 shrink-0 rounded-full" style={{ background: p.color }} /><span className="min-w-0 flex-1 break-all text-muted-foreground">{p.name}</span><span className="shrink-0 font-medium tabular-nums">{seriesFormat(series.find((s) => s.key === p.dataKey))(Number(p.value))}</span></div>)}</div>{stacked && !secondaryAxis && points.length > 1 && <div className="mt-2 flex justify-between gap-4 border-t border-border pt-2 font-semibold"><span>{t('charts:visibleTotal')}</span><span>{format(points.reduce((sum, p) => sum + Number(p.value), 0))}</span></div>}</div>
              }} />
              {visible.map((s) => {
                const shared = { dataKey: s.key, name: s.label, stroke: s.color, isAnimationActive: false, yAxisId: s.axis === 'right' && secondaryAxis ? 'right' : 0 }
                return line || (s.axis === 'right' && secondaryAxis) ? <Line key={s.key} {...shared} type="linear" strokeWidth={2} strokeDasharray={s.axis === 'right' ? '5 3' : undefined} dot={data.length <= 31 ? { r: 2 } : false} activeDot={{ r: 4, strokeWidth: 2, stroke: 'var(--color-card)' }} connectNulls={false} />
                  : type === 'bar' ? <Bar key={s.key} {...shared} stroke="none" fill={s.color} stackId={stacked ? 'usage' : undefined} maxBarSize={32} radius={stacked ? undefined : [3, 3, 0, 0]} />
                    : <Area key={s.key} {...shared} type="linear" strokeWidth={2} fill={`url(#${id}-${series.indexOf(s)})`} stackId={stacked ? 'usage' : undefined} connectNulls={false} dot={data.length === 1 ? { r: 4 } : false} activeDot={{ r: 3 }} />
              })}
            </ComposedChart>
          </ResponsiveContainer>
        </div>
      )}
      {!secondaryAxis && (!compact || series.length > 1) && legend}
    </div>
  )
}
