import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { TagInput } from '@/components/ui/tag-input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

export function GroupsSection({
  userId,
  current,
  onDone,
}: {
  userId: number
  current: string[]
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [codes, setCodes] = useState<string[]>(current)

  // 覆盖式设置分组：后端按 (group_code, priority) 全量替换，故 UI 也是全量提交
  const setGroups = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/users/${userId}/groups`, {
        method: 'POST',
        body: {
          groups: codes.map((group_code, idx) => ({
            group_code,
            priority: codes.length - idx,
          })),
        },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <FieldGroup title={t('admin:userGroups')} hint={t('admin:userGroupsHint')}>
      <TagInput value={codes} onChange={setCodes} placeholder={t('admin:tagInputHint')} />
      <Button
        size="sm"
        variant="outline"
        className="self-start"
        disabled={setGroups.isPending}
        onClick={() => setGroups.mutate()}
      >
        {t('common:save')}
      </Button>
    </FieldGroup>
  )
}
