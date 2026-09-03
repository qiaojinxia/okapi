import { useTranslation } from 'react-i18next'
import {
  Bar,
  BarChart,
  CartesianGrid,
  Legend,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/state'
import type { BreakdownRow } from '@/features/portal-overview/types'
import { sumByModel } from '@/features/portal-overview/types'
import { OTHER_COLOR, OTHER_KEY, chartColor } from '@/lib/chart'

const TOP_N = 6

/// 按模型堆叠的按日消费（new-api 数据看板主图）。
///
/// 此前门户只有单色的按日总额柱——回答"何时花的"却答不了"花在哪"。
/// Top N 按窗口消耗取，其余折叠为"其他"；用户自己的数据量有限，折叠在前端做。
export function SpendTrendView({ rows }: { rows: BreakdownRow[] }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language

  const ranked = [...sumByModel(rows).entries()]
    .sort((a, b) => b[1].amount_micro - a[1].amount_micro)
    .map(([m]) => m)
  const top = ranked.slice(0, TOP_N)
  const hasOther = ranked.length > top.length
  const series = hasOther ? [...top, OTHER_KEY] : top

  // day → { model: usd }
  const byDay = new Map<string, Record<string, number>>()
  for (const r of rows) {
    const slot = top.includes(r.model) ? r.model : OTHER_KEY
    const cell = byDay.get(r.day) ?? {}
    cell[slot] = (cell[slot] ?? 0) + r.amount_micro / 1_000_000
    byDay.set(r.day, cell)
  }
  const chart = [...byDay.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([day, cells]) => ({ day: day.slice(5), ...cells }))

  if (chart.length === 0) {
    return (
      <Card>
        <CardContent>
          <EmptyState hint={t('portal:emptyUsageHint')} />
        </CardContent>
      </Card>
    )
  }
  const label = (m: string) => (m === OTHER_KEY ? t('admin:statOtherModels') : m)
  return (
    <Card>
      <CardContent className="h-80 pt-6">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart data={chart}>
            <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
            <XAxis dataKey="day" fontSize={11} />
            <YAxis fontSize={11} />
            <Tooltip
              formatter={(value, name) => {
                const n = typeof value === 'number' ? value : Number(value ?? 0)
                return [
                  n.toLocaleString(locale, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 }),
                  String(name),
                ]
              }}
            />
            {/* 同管理端模型消耗图：关掉 Recharts 3 的字母序图例，保持消费降序 */}
            <Legend itemSorter={null} />
            {series.map((m, idx) => (
              <Bar
                key={m}
                dataKey={m}
                name={label(m)}
                stackId="spend"
                fill={m === OTHER_KEY ? OTHER_COLOR : chartColor(idx)}
                isAnimationActive={false}
              />
            ))}
          </BarChart>
        </ResponsiveContainer>
      </CardContent>
    </Card>
  )
}
