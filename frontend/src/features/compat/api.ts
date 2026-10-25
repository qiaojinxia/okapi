import { type QueryClient, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type {
  Catalog,
  CompatRule,
  DryRunBody,
  DryRunResult,
  InboxItem,
  ProfileDetail,
  ProfileRow,
  SetOverview,
  SetScope,
  VersionRow,
} from '@/features/compat/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

/// 接口里护栏的 `{id}` 写 0。
export function setPath(scope: SetScope, id: number): string {
  return `/admin/compat/sets/${scope}/${scope === 'guard' ? 0 : id}`
}

/// 目录读不到就当空：蓝图 / 配方 / 参考页只是少一点东西，不该整页崩掉。
function normalizeCatalog(raw: Partial<Catalog> | null | undefined): Catalog {
  return {
    schema_version: raw?.schema_version ?? 1,
    phases: raw?.phases ?? [],
    ops: raw?.ops ?? [],
    selector: {
      fields: raw?.selector?.fields ?? [],
      capability_fields: raw?.selector?.capability_fields ?? [],
      response_dialects: raw?.selector?.response_dialects ?? [],
    },
    when: raw?.when ?? { fields: [], semantics: '' },
    usage_slots: raw?.usage_slots ?? [],
    capability_facts: raw?.capability_facts ?? [],
    protected_paths: {
      request: raw?.protected_paths?.request ?? [],
      response: raw?.protected_paths?.response ?? [],
    },
    limits: raw?.limits ?? {},
    recipes: raw?.recipes ?? [],
  }
}

export function useCatalog() {
  return useQuery({
    queryKey: qk.compatCatalog,
    staleTime: 300_000,
    queryFn: async () => normalizeCatalog(await apiFetch<Partial<Catalog>>('/admin/compat/catalog')),
  })
}

export function useProfiles() {
  return useQuery({
    queryKey: qk.compatProfiles,
    queryFn: async () => {
      const raw = await apiFetch<{ data?: ProfileRow[] }>('/admin/compat/profiles')
      return (raw.data ?? []).map((p) => ({ ...p, hosts: p.hosts ?? [] }))
    },
  })
}

export function useProfile(id: number | undefined) {
  return useQuery({
    queryKey: qk.compatProfile(id ?? 0),
    enabled: id !== undefined,
    queryFn: () => apiFetch<ProfileDetail>(`/admin/compat/profiles/${id}`),
  })
}

export function useSetOverview(scope: SetScope, id: number, enabled = true) {
  return useQuery({
    queryKey: qk.compatSet(scope, id),
    enabled,
    queryFn: async (): Promise<SetOverview> => {
      const raw = await apiFetch<Partial<SetOverview>>(setPath(scope, id))
      return {
        scope,
        id,
        published: raw.published ?? null,
        draft: raw.draft ?? null,
        stale: raw.stale ?? false,
      }
    },
  })
}

export function useVersions(scope: SetScope, id: number, enabled: boolean) {
  return useQuery({
    queryKey: qk.compatVersions(scope, id),
    enabled,
    queryFn: async () => (await apiFetch<{ data?: VersionRow[] }>(`${setPath(scope, id)}/versions`)).data ?? [],
  })
}

/// 某一版的完整内容（历史页展开时才取）。
export function useVersionDetail(scope: SetScope, id: number, version: number | null) {
  return useQuery({
    queryKey: [...qk.compatVersions(scope, id), version] as const,
    enabled: version !== null,
    queryFn: () => apiFetch<{ version: number; rules: CompatRule[] }>(`${setPath(scope, id)}/versions/${version}`),
  })
}

export function useInbox(enabled: boolean) {
  return useQuery({
    queryKey: qk.compatInbox,
    enabled,
    refetchInterval: 60_000,
    queryFn: async () => (await apiFetch<{ data?: InboxItem[] }>('/admin/compat/drafts')).data ?? [],
  })
}

/// 规则集一动，所有读它的视图都要刷：概览 / 历史 / 收件箱 / 档案列表与详情，渠道层还有抽屉里的有效规则。
export function invalidateCompat(queryClient: QueryClient, scope: SetScope, id: number) {
  const keys = [
    qk.compatSet(scope, id),
    qk.compatVersions(scope, id),
    qk.compatInbox,
    qk.compatProfiles,
    qk.compatProfile(id),
  ]
  for (const queryKey of keys) void queryClient.invalidateQueries({ queryKey })
  if (scope === 'channel') void queryClient.invalidateQueries({ queryKey: qk.channelCompat(id) })
  // 护栏 / 档案影响所有（选了它的）渠道的有效规则
  if (scope !== 'channel') void queryClient.invalidateQueries({ queryKey: ['admin', 'channel-compat'] })
}

export interface DraftPayload {
  rules: CompatRule[]
  base_version: number
  note?: string
  rationale?: string
}

/// 一个规则集上的全部写操作；成功后统一刷新。调用方自己决定 toast 与后续。
export function useSetActions(scope: SetScope, id: number) {
  const queryClient = useQueryClient()
  const path = setPath(scope, id)
  const done = () => invalidateCompat(queryClient, scope, id)
  return {
    saveDraft: useMutation({
      mutationFn: (body: DraftPayload) => apiFetch<{ ok: boolean; version: number }>(`${path}/draft`, { method: 'PUT', body }),
      onSuccess: done,
    }),
    discardDraft: useMutation({
      mutationFn: () => apiFetch(`${path}/draft`, { method: 'DELETE' }),
      onSuccess: done,
    }),
    publishDraft: useMutation({
      mutationFn: () => apiFetch<{ ok: boolean; version: number }>(`${path}/draft/publish`, { method: 'POST' }),
      onSuccess: done,
    }),
    rollback: useMutation({
      mutationFn: (version: number) =>
        apiFetch<{ ok: boolean; version: number }>(`${path}/rollback`, { method: 'POST', body: { version } }),
      onSuccess: done,
    }),
    publishDirect: useMutation({
      mutationFn: (body: { rules: CompatRule[]; base_version: number; note?: string }) =>
        apiFetch<{ ok: boolean; version: number }>(`${path}/rules`, { method: 'PUT', body }),
      onSuccess: done,
    }),
  }
}

export function useDryRun() {
  return useMutation({
    mutationFn: (body: DryRunBody) => apiFetch<DryRunResult>('/admin/compat/dry-run', { method: 'POST', body }),
  })
}

export interface ProfileInput {
  slug?: string
  name?: string
  description?: string | null
  extends?: string | null
  hosts?: string[]
}

export function useProfileActions() {
  const queryClient = useQueryClient()
  const done = () => {
    void queryClient.invalidateQueries({ queryKey: qk.compatProfiles })
    void queryClient.invalidateQueries({ queryKey: ['admin', 'compat-profile'] })
    // 渠道抽屉的厂商下拉读的是同一份目录
    void queryClient.invalidateQueries({ queryKey: qk.compatVendors })
  }
  return {
    create: useMutation({
      mutationFn: (body: ProfileInput) => apiFetch<ProfileRow>('/admin/compat/profiles', { method: 'POST', body }),
      onSuccess: done,
    }),
    update: useMutation({
      mutationFn: (arg: { id: number; body: ProfileInput }) =>
        apiFetch<ProfileRow>(`/admin/compat/profiles/${arg.id}`, { method: 'PATCH', body: arg.body }),
      onSuccess: done,
    }),
    remove: useMutation({
      mutationFn: (id: number) => apiFetch(`/admin/compat/profiles/${id}`, { method: 'DELETE' }),
      onSuccess: done,
    }),
  }
}
