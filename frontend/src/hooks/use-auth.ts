import { useQuery } from '@tanstack/react-query'
import { apiFetch, getKey } from '@/lib/api'
import { qk } from '@/lib/query-keys'

export interface Me {
  user_id: number
  key_id: number
  group: string
  balance_micro: number
}

export function useMe() {
  return useQuery({
    queryKey: qk.me,
    queryFn: () => apiFetch<Me>('/api/me'),
    enabled: getKey() !== null,
    staleTime: 15_000,
  })
}
