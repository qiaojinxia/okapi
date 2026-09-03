/// 图表分类色板：引设计令牌（index.css `--color-chart-*`），亮/暗主题各一套亮度，
/// 组件禁硬编码色值的规则对图表同样成立。堆叠图按序列顺序取色，超出 8 档循环。
export const CHART_PALETTE = [
  'var(--color-chart-1)',
  'var(--color-chart-2)',
  'var(--color-chart-3)',
  'var(--color-chart-4)',
  'var(--color-chart-5)',
  'var(--color-chart-6)',
  'var(--color-chart-7)',
  'var(--color-chart-8)',
]

/// Top N 之外的折叠桶：恒用中性灰——"其他"不该比任何具名序列显眼。
export const OTHER_KEY = '__other'
export const OTHER_COLOR = 'var(--color-muted-foreground)'

export function chartColor(idx: number): string {
  return CHART_PALETTE[idx % CHART_PALETTE.length]
}
