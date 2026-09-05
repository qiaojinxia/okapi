import { Pencil, Plus, Shield, Trash2 } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { RoleDrawer } from '@/features/roles/RoleDrawer'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

interface RoleRow {
  id: number
  role_code: string
  display_name: string
  permissions: unknown
}


/// 角色与权限页。
///
/// 与用户列表分开：给谁什么角色是日常操作，定义角色本身是低频的结构性改动，
/// 混在一页会让人误以为改角色只影响当前选中的用户。
export function RolesPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [drawer, setDrawer] = useState<'create' | RoleRow | null>(null)
  const { confirm, dialog } = useConfirm()

  const roles = useQuery({
    queryKey: qk.adminRoles,
    queryFn: () => apiFetch<{ data: RoleRow[] }>('/admin/roles'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminRoles })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/roles/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    // 仍有用户绑定时后端回 409 role_in_use，此处直接展示 error_code 的语言包文案
    onError: (err) => toast.error(describeError(err)),
  })

  const rows = roles.data?.data ?? []
  const permCount = (p: unknown) => (Array.isArray(p) ? p.length : 0)

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        icon={Shield}
        title={t('admin:rolesTitle')}
        description={t('admin:roleHint')}
        meta={<Badge variant="muted">{t('admin:keyTotal', { n: rows.length })}</Badge>}
        action={
          <Button onClick={() => setDrawer('create')}>
            <Plus className="h-4 w-4" />
            {t('admin:roleCreate')}
          </Button>
        }
      />
      {dialog}

      {roles.isError ? (
        <ErrorState message={describeError(roles.error)} onRetry={() => void roles.refetch()} />
      ) : roles.isPending ? (
        <TableSkeleton rows={6} cols={5} />
      ) : rows.length === 0 ? (
        <EmptyState
          hint={t('admin:rolesEmptyHint')}
          action={
            <Button onClick={() => setDrawer('create')}>
              <Plus className="h-4 w-4" />
              {t('admin:roleCreate')}
            </Button>
          }
        />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('admin:roleCode')}</Th>
              <Th>{t('admin:roleName')}</Th>
              <Th>{t('admin:rolePermissionsCol')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => (
              <Tr key={r.id}>
                <Td>{r.id}</Td>
                <Td className="font-mono text-xs">{r.role_code}</Td>
                <Td>{r.display_name}</Td>
                <Td>
                  <div className="flex flex-wrap gap-1">
                    {Array.isArray(r.permissions) &&
                      (r.permissions as string[]).slice(0, 4).map((p) => (
                        <Badge key={p} variant="muted" className="font-mono">
                          {p}
                        </Badge>
                      ))}
                    {permCount(r.permissions) > 4 && (
                      <Badge variant="muted">
                        {t('admin:permMore', { n: permCount(r.permissions) - 4 })}
                      </Badge>
                    )}
                  </div>
                </Td>
                <Td>
                  <div className="flex items-center gap-0.5">
                    <IconButton
                      icon={Pencil}
                      label={t('common:edit')}
                      onClick={() => setDrawer(r)}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: r.role_code }),
                          description: t('common:confirmRoleDelete'),
                          onConfirm: () => remove.mutate(r.role_code),
                        })
                      }
                    />
                  </div>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {drawer !== null && (
        <RoleDrawer
          initial={drawer === 'create' ? undefined : drawer}
          onClose={() => setDrawer(null)}
          onDone={() => {
            toast.success(t('common:success'))
            invalidate()
          }}
        />
      )}
    </div>
  )
}
