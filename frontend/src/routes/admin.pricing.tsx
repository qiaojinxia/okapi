import { createFileRoute } from '@tanstack/react-router'
import { ModelPricingPage } from '@/features/models/ModelPricingPage'
import { type PageSearch, pageSearch } from '@/hooks/use-pagination'
import { flag, text } from '@/lib/search-params'

export interface ModelsSearch extends PageSearch {
  /// 关键词（模型名 / 展示名）。
  q?: string
  /// 只看未定价模型。
  unpriced?: true
}

export const Route = createFileRoute('/admin/pricing')({
  validateSearch: (search: Record<string, unknown>): ModelsSearch => ({
    ...pageSearch(search),
    q: text(search.q),
    unpriced: flag(search.unpriced),
  }),
  component: ModelPricingPage,
})
