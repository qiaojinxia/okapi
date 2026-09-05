import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Area, CartesianGrid, ComposedChart, Bar, Line, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'
import { Download, Table2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Segmented } from '@/components/ui/segmented'
import { downloadCsv } from '@/lib/csv'
import { cn } from '@/lib/utils'

export interface ChartSeries { key: string; label: string; color: string }
export interface ChartPoint { bucket: string; [key: string]: string | number | null }

export function chartNumber(n: number, locale: string): string {
  return new Intl.NumberFormat(locale, { notation: Math.abs(n) >= 10_000 ? 'compact' : 'standard', maximumFractionDigits: 2 }).format(n)
}

export function TimeChart({ data, series, format, unit, label, stacked = false, line = false, percent = false, defaultType = 'area' }: {
  data: ChartPoint[]
  series: ChartSeries[]
  format: (value: number) => string
  unit: string
  label: string
  stacked?: boolean
  line?: boolean
  percent?: boolean
  defaultType?: 'area' | 'bar'
}) {
  const { t, i18n } = useTranslation()
  const id = useId().replaceAll(':', '')
  const [type, setType] = useState<'area' | 'bar'>(defaultType)
  const [table, setTable] = useState(false)
  const [hidden, setHidden] = useState<string[]>([])
  const visible = series.filter((s) => !hidden.includes(s.key))
  const exportRows = () => downloadCsv('usage-chart', [t('charts:date'), ...series.map((s) => `${s.label} (${unit})`)], data.map((d) => [d.bucket, ...series.map((s) => d[s.key])]))
  return (
    <div className="min-w-0 space-y-4" role="group" aria-label={label}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <span className="text-xs text-muted-foreground">{t('charts:unit', { unit })}</span>
        <div className="flex flex-wrap items-center gap-2">
          {!line && <Segmented size="sm" ariaLabel={t('charts:type')} value={type} onChange={setType} options={[{ value: 'area', label: t('charts:area') }, { value: 'bar', label: t('charts:bar') }]} />}
          <Button variant="ghost" size="sm" aria-pressed={table} onClick={() => setTable(!table)}><Table2 className="h-3.5 w-3.5" />{t('charts:table')}</Button>
          <Button variant="outline" size="sm" onClick={exportRows}><Download className="h-3.5 w-3.5" />{t('charts:export')}</Button>
        </div>
      </div>
      {table ? (
        <div className="max-h-80 overflow-auto rounded-lg border border-border">
          <table className="w-full text-xs tabular-nums">
            <caption className="sr-only">{label}</caption>
            <thead className="sticky top-0 bg-muted"><tr><th className="px-3 py-2 text-left">{t('charts:date')}</th>{series.map((s) => <th key={s.key} className="px-3 py-2 text-right whitespace-nowrap">{s.label}</th>)}</tr></thead>
            <tbody>{data.map((point) => <tr key={point.bucket} className="border-t border-border/60"><td className="px-3 py-2 whitespace-nowrap">{point.bucket}</td>{series.map((s) => <td key={s.key} className="px-3 py-2 text-right whitespace-nowrap">{typeof point[s.key] === 'number' ? format(point[s.key] as number) : '—'}</td>)}</tr>)}</tbody>
          </table>
        </div>
      ) : (
        <div className="h-72 min-w-0 sm:h-80" aria-label={t('charts:plot')}>
          <ResponsiveContainer width="100%" height="100%" minWidth={0}>
            <ComposedChart data={data} margin={{ top: 10, right: 10, bottom: 4, left: 0 }} accessibilityLayer>
              <defs>{series.map((s, i) => <linearGradient key={s.key} id={`${id}-${i}`} x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor={s.color} stopOpacity={0.28} /><stop offset="100%" stopColor={s.color} stopOpacity={0.025} /></linearGradient>)}</defs>
              <CartesianGrid vertical={false} stroke="var(--color-border)" strokeDasharray="3 5" strokeOpacity={0.7} />
              <XAxis dataKey="bucket" tickFormatter={(value: string) => value.length > 10 ? `${value.slice(5, 10)} ${value.slice(11, 16)}` : value.slice(5)} tick={{ fontSize: 11, fill: 'var(--color-muted-foreground)' }} axisLine={false} tickLine={false} minTickGap={32} tickMargin={10} />
              <YAxis width={54} domain={percent ? [0, 100] : [0, 'auto']} tick={{ fontSize: 11, fill: 'var(--color-muted-foreground)' }} tickFormatter={(n) => `${chartNumber(Number(n), i18n.language)}${percent ? '%' : ''}`} axisLine={false} tickLine={false} tickMargin={8} />
              <Tooltip cursor={{ stroke: 'var(--color-muted-foreground)', strokeDasharray: '4 4', fill: 'var(--color-muted)', fillOpacity: 0.25 }} content={({ active, payload, label: date }) => {
                if (!active || !payload?.length) return null
                const points = payload.filter((p) => typeof p.value === 'number')
                return <div className="max-w-[min(22rem,80vw)] rounded-xl border border-border bg-popover p-3 text-xs text-popover-foreground shadow-popover"><p className="mb-2 font-medium">{String(date)}</p><div className="max-h-56 space-y-2 overflow-auto">{points.map((p) => <div key={String(p.dataKey)} className="flex items-center gap-2"><span className="h-2 w-2 shrink-0 rounded-full" style={{ background: p.color }} /><span className="min-w-0 flex-1 break-all text-muted-foreground">{p.name}</span><span className="shrink-0 font-medium tabular-nums">{format(Number(p.value))}</span></div>)}</div>{stacked && points.length > 1 && <div className="mt-2 flex justify-between gap-4 border-t border-border pt-2 font-semibold"><span>{t('charts:visibleTotal')}</span><span>{format(points.reduce((sum, p) => sum + Number(p.value), 0))}</span></div>}</div>
              }} />
              {visible.map((s) => {
                const shared = { dataKey: s.key, name: s.label, stroke: s.color, isAnimationActive: false }
                return line ? <Line key={s.key} {...shared} type="linear" strokeWidth={2} dot={data.length <= 31 ? { r: 2 } : false} activeDot={{ r: 4, strokeWidth: 2, stroke: 'var(--color-card)' }} connectNulls={false} />
                  : type === 'bar' ? <Bar key={s.key} {...shared} stroke="none" fill={s.color} stackId={stacked ? 'usage' : undefined} maxBarSize={32} radius={stacked ? undefined : [3, 3, 0, 0]} />
                    : <Area key={s.key} {...shared} type="linear" strokeWidth={2} fill={`url(#${id}-${series.indexOf(s)})`} stackId={stacked ? 'usage' : undefined} connectNulls={false} dot={data.length === 1 ? { r: 4 } : false} activeDot={{ r: 3 }} />
              })}
            </ComposedChart>
          </ResponsiveContainer>
        </div>
      )}
      <div role="group" aria-label={t('charts:legend')} className="flex flex-wrap gap-1.5 border-t border-border/60 pt-3">
        {series.map((s) => <button type="button" key={s.key} aria-pressed={!hidden.includes(s.key)} disabled={!hidden.includes(s.key) && visible.length === 1} onClick={() => setHidden((previous) => previous.includes(s.key) ? previous.filter((key) => key !== s.key) : [...previous, s.key])} className={cn('inline-flex min-h-8 max-w-full items-center gap-2 rounded-md px-2 text-xs outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-default', hidden.includes(s.key) && 'opacity-45')}><span className="h-2 w-2 shrink-0 rounded-full" style={{ background: s.color }} /><span className="truncate">{s.label}</span></button>)}
      </div>
    </div>
  )
}
