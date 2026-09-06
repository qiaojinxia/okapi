import { useId } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Input } from '@/components/ui/input'
import { TagInput } from '@/components/ui/tag-input'
import { SearchInput } from '@/components/ui/search-input'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

export function useConfiguredModels() {
  const can = usePermission()
  return useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: { model_name: string; display_name?: string | null; vendor?: string | null }[] }>('/admin/models'),
    staleTime: 60_000,
    enabled: can('pricing.read'),
  })
}

export function ModelSearchInput(props: React.ComponentProps<typeof SearchInput>) {
  const models = useConfiguredModels()
  const listId = useId()
  const search = props.value.trim().toLowerCase()
  return <>
    <SearchInput {...props} list={listId} autoComplete="off" />
    <datalist id={listId}>
      {(models.data?.data ?? []).filter((model) => `${model.model_name} ${model.display_name ?? ''} ${model.vendor ?? ''}`.toLowerCase().includes(search)).slice(0, 50).map((model) => <option key={model.model_name} value={model.model_name} label={[model.display_name, model.vendor].filter(Boolean).join(' · ')} />)}
    </datalist>
  </>
}

// 使用当前站点的模型目录；仍允许自定义模型名与尚未接入的模型。
export function ModelInput(props: React.ComponentProps<typeof Input>) {
  const models = useConfiguredModels()
  const listId = useId()
  const search = String(props.value ?? '').trim().toLowerCase()
  return <>
    <Input {...props} list={props.readOnly ? undefined : listId} autoComplete="off" />
    <datalist id={listId}>
      {(models.data?.data ?? []).filter((model) => `${model.model_name} ${model.display_name ?? ''} ${model.vendor ?? ''}`.toLowerCase().includes(search)).slice(0, 50).map((model) => <option key={model.model_name} value={model.model_name} label={[model.display_name, model.vendor].filter(Boolean).join(' · ')} />)}
    </datalist>
  </>
}

export function ModelTagsInput(props: React.ComponentProps<typeof TagInput>) {
  const models = useConfiguredModels()
  return <TagInput {...props} suggestions={(models.data?.data ?? []).map((model) => model.model_name)} />
}
