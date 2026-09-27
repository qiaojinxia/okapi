import { EntitySearchInput, validEntityId } from '@/features/entity-search/EntitySearchInput'
import type { EntitySearchInputProps } from '@/features/entity-search/EntitySearchInput'

interface UserOption { id: number; username: string; email?: string | null }
type Props = Omit<EntitySearchInputProps, 'kind' | 'knownOptions'> & { knownUsers?: UserOption[] }

export const validUserFilter = validEntityId

export function UserSearchInput({ knownUsers = [], ...props }: Props) {
  return <EntitySearchInput {...props} kind="user" knownOptions={knownUsers.map((user) => ({ id: user.id, name: user.username, description: user.email ?? undefined }))} />
}
