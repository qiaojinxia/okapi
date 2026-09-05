import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { ErrorState } from '@/components/ui/state'
import { Input, Label } from '@/components/ui/input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 权限点按前缀分组展示（channel.* / pricing.* / user.* …）。
/// 二十多个点平铺成一片勾选框时，用户无法判断"配了这些够不够"；按资源分组后
/// 每组读写成对出现，缺哪个一眼可见。
export function groupPermissions(all: string[]): [string, string[]][] {
  const groups = new Map<string, string[]>()
  for (const p of all) {
    const domain = p.split('.')[0] ?? p
    groups.set(domain, [...(groups.get(domain) ?? []), p])
  }
  return [...groups.entries()].sort(([a], [b]) => a.localeCompare(b))
}


/// 编辑态回填所需字段。
export interface RoleInitial {
  role_code: string
  display_name: string
  permissions: unknown
}

/// 角色抽屉；`initial` 给出即为编辑（role_code 锁定，后端按 code upsert 并全量失效鉴权缓存）。
export function RoleDrawer({
  onClose,
  onDone,
  initial,
}: {
  onClose: () => void
  onDone: () => void
  initial?: RoleInitial
}) {
  const { t } = useTranslation()
  const editing = initial !== undefined
  const [form, setForm] = useState({
    role_code: initial?.role_code ?? '',
    display_name: initial?.display_name ?? '',
  })
  const [picked, setPicked] = useState<Set<string>>(
    new Set(Array.isArray(initial?.permissions) ? (initial.permissions as string[]) : []),
  )
  // 权限点清单由后端导出，避免前端硬编码字符串与后端漂移
  const permissions = useQuery({
    queryKey: qk.adminPermissions,
    queryFn: () => apiFetch<{ data: string[] }>('/admin/permissions'),
  })

  const create = useMutation({
    mutationFn: () =>
      apiFetch<{ role_id: number }>('/admin/roles', {
        method: 'POST',
        body: {
          role_code: form.role_code.trim(),
          display_name: form.display_name.trim(),
          permissions: [...picked],
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const toggle = (p: string) =>
    setPicked((prev) => {
      const next = new Set(prev)
      if (next.has(p)) next.delete(p)
      else next.add(p)
      return next
    })

  const grouped = groupPermissions(permissions.data?.data ?? [])

  return (
    <Drawer
      open
      onClose={onClose}
      title={editing ? t('admin:roleEdit', { code: initial.role_code }) : t('admin:roleCreate')}
      description={t('admin:roleDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={create.isPending || form.role_code.trim() === '' || picked.size === 0}
            onClick={() => create.mutate()}
          >
            {t('common:create')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')}>
        <div className="grid grid-cols-2 gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rcode">{t('admin:roleCode')}</Label>
            <Input
              id="rcode"
              className="font-mono text-sm"
              value={form.role_code}
              placeholder="ops_readonly"
              disabled={editing}
              onChange={(e) => setForm((f) => ({ ...f, role_code: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rname">{t('admin:roleName')}</Label>
            <Input
              id="rname"
              value={form.display_name}
              onChange={(e) => setForm((f) => ({ ...f, display_name: e.target.value }))}
            />
          </div>
        </div>
      </FieldGroup>

      <FieldGroup
        title={t('admin:rolePermissions')}
        hint={t('admin:permPickedCount', { n: picked.size })}
      >
        {permissions.isError && <ErrorState message={describeError(permissions.error)} />}
        {grouped.map(([domain, perms]) => (
          <div key={domain} className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <span className="text-xs font-medium">{domain}</span>
              <button
                type="button"
                className="text-xs text-muted-foreground underline"
                onClick={() =>
                  setPicked((prev) => {
                    const next = new Set(prev)
                    const allOn = perms.every((p) => next.has(p))
                    for (const p of perms) {
                      if (allOn) next.delete(p)
                      else next.add(p)
                    }
                    return next
                  })
                }
              >
                {t('admin:permToggleGroup')}
              </button>
            </div>
            <div className="grid grid-cols-2 gap-1">
              {perms.map((p) => (
                <Checkbox
                  key={p}
                  label={p}
                  checked={picked.has(p)}
                  onChange={() => toggle(p)}
                  className="font-mono text-xs"
                />
              ))}
            </div>
          </div>
        ))}
      </FieldGroup>
    </Drawer>
  )
}
