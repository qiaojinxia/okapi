import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { RoleRow } from '@/features/users/types'
import { BUILTIN_ROLES, roleLabel } from '@/features/users/types'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

export function RoleSection({
  userId,
  roles,
  onDone,
}: {
  userId: number
  roles: RoleRow[]
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [role, setRole] = useState('')
  const [adminRoleId, setAdminRoleId] = useState('')

  const assign = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/users/${userId}/role`, {
        method: 'POST',
        body: {
          role: role === '' ? undefined : Number(role),
          admin_role_id: adminRoleId === '' ? undefined : Number(adminRoleId),
        },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <FieldGroup title={t('admin:usersRole')} hint={t('admin:roleAssignHint')}>
      <div className="flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="role">{t('admin:roleBuiltin')}</Label>
          <Select
            id="role"
            className="w-36"
            value={role}
            onChange={setRole}
            placeholder={t('admin:roleKeep')}
            options={BUILTIN_ROLES.map((r) => ({ value: String(r), label: roleLabel(r, t) }))}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="arole">{t('admin:roleCustom')}</Label>
          <Select
            id="arole"
            className="w-52"
            value={adminRoleId}
            onChange={setAdminRoleId}
            placeholder={t('admin:roleKeep')}
            options={roles.map((r) => ({
              value: String(r.id),
              label: `${r.display_name} (${r.role_code})`,
            }))}
          />
        </div>
        <Button
          size="sm"
          variant="outline"
          disabled={assign.isPending || (role === '' && adminRoleId === '')}
          onClick={() => assign.mutate()}
        >
          {t('admin:roleAssign')}
        </Button>
      </div>
    </FieldGroup>
  )
}
