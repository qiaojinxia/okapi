import { useQuery } from '@tanstack/react-query'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import type { AutocompleteInputProps } from '@/components/ui/autocomplete-input'
import { TagInput } from '@/components/ui/tag-input'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import type { PricingModel } from '@/features/public-pricing/types'

interface ConfiguredModel {
  model_name: string
  display_name?: string | null
  vendor?: string | null
}

export function useConfiguredModels() {
  const can = usePermission()
  return useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: ConfiguredModel[] }>('/admin/models'),
    staleTime: 60_000,
    enabled: can('pricing.read'),
  })
}

const suggestion = (model: ConfiguredModel) => ({
  value: model.model_name,
  label: model.display_name ?? undefined,
  description: model.vendor ?? undefined,
})

type ModelInputProps = Omit<AutocompleteInputProps, 'options' | 'loading' | 'error'>

export function ModelSearchInput(props: ModelInputProps) {
  return <ModelInput {...props} search />
}

// 门户只使用公开模型目录，不依赖管理员的定价读取权限。
export function PublicModelSearchInput(props: ModelInputProps) {
  const models = useQuery({
    queryKey: qk.publicPricing,
    queryFn: () => apiFetch<{ models: PricingModel[] }>('/api/pricing'),
    staleTime: 60_000,
    retry: false,
  })
  return <AutocompleteInput {...props} search
    options={(models.data?.models ?? []).map((model) => ({ value: model.model, label: model.display_name ?? undefined, description: model.vendor ?? undefined }))}
    loading={models.isLoading}
    error={models.isError ? describeError(models.error) : undefined}
  />
}

// 只提供已有模型建议，允许手输新模型；选择时始终填入准确 ID，不提交展示名。
export function ModelInput(props: ModelInputProps) {
  const models = useConfiguredModels()
  return <AutocompleteInput {...props}
    options={(models.data?.data ?? []).map(suggestion)}
    loading={models.isLoading}
    error={models.isError ? describeError(models.error) : undefined}
  />
}

export function ModelTagsInput(props: React.ComponentProps<typeof TagInput>) {
  const models = useConfiguredModels()
  return <TagInput {...props} suggestions={(models.data?.data ?? []).map(suggestion)} />
}
