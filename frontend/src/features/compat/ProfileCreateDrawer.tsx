import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { Input, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { TagInput } from '@/components/ui/tag-input'
import { toast } from '@/components/ui/toast'
import { useCompatVendors } from '@/features/channels/compat'
import { useProfileActions } from '@/features/compat/api'
import type { ProfileRow } from '@/features/compat/types'
import { describeError } from '@/lib/i18n'

/// 与后端 `ensure_slug` 同一形状：小写字母 / 数字开头，之后可带 `. _ -`，至多 64 字符。
const SLUG = /^[a-z0-9][a-z0-9._-]{0,63}$/

/// 新建适配档案：一份可复用的「这类上游怎么处理」，渠道的厂商字段选它就生效。
/// 建好后规则从空开始写（档案的规则要走草稿 / 发布），所以这里只有身份信息。
export function ProfileCreateDrawer({
  open,
  onClose,
  onCreated,
}: {
  open: boolean
  onClose: () => void
  onCreated: (profile: ProfileRow) => void
}) {
  if (!open) return null
  return <CreateForm onClose={onClose} onCreated={onCreated} />
}

function CreateForm({ onClose, onCreated }: { onClose: () => void; onCreated: (profile: ProfileRow) => void }) {
  const { t } = useTranslation()
  const vendors = useCompatVendors(true)
  const actions = useProfileActions()
  const [slug, setSlug] = useState('')
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [extendsVendor, setExtendsVendor] = useState('')
  const [hosts, setHosts] = useState<string[]>([])
  const [touched, setTouched] = useState(false)

  const slugBad = !SLUG.test(slug.trim().toLowerCase())
  const nameBad = name.trim() === ''
  const submit = () => {
    setTouched(true)
    if (slugBad || nameBad) return
    actions.create.mutate(
      {
        slug: slug.trim().toLowerCase(),
        name: name.trim(),
        ...(description.trim() === '' ? {} : { description: description.trim() }),
        ...(extendsVendor === '' ? {} : { extends: extendsVendor }),
        hosts,
      },
      {
        onSuccess: (profile) => {
          toast.success(t('compat:profileCreated'))
          onCreated(profile)
        },
        onError: (err) => toast.error(describeError(err)),
      },
    )
  }

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('compat:profileCreate')}
      description={t('compat:profileCreateHint')}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button loading={actions.create.isPending} onClick={submit}>
            {t('compat:profileCreate')}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field
          label={t('compat:fieldSlug')}
          htmlFor="new-profile-slug"
          required
          hint={t('compat:fieldSlugHint')}
          error={touched && slugBad ? t('compat:slugInvalid') : null}
        >
          <Input
            id="new-profile-slug"
            value={slug}
            maxLength={64}
            autoComplete="off"
            placeholder="acme-relay"
            aria-invalid={touched && slugBad}
            onChange={(e) => setSlug(e.target.value)}
          />
        </Field>
        <Field label={t('compat:fieldName')} htmlFor="new-profile-name" required error={touched && nameBad ? t('compat:nameRequired') : null}>
          <Input
            id="new-profile-name"
            value={name}
            maxLength={128}
            placeholder="Acme Relay"
            aria-invalid={touched && nameBad}
            onChange={(e) => setName(e.target.value)}
          />
        </Field>
        <Field label={t('compat:fieldDescription')} htmlFor="new-profile-description">
          <Textarea id="new-profile-description" value={description} maxLength={512} rows={3} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <Field label={t('compat:fieldExtends')} htmlFor="new-profile-extends" hint={t('compat:fieldExtendsHint')}>
          <Select
            id="new-profile-extends"
            className="w-72"
            value={extendsVendor}
            placeholder={t('compat:extendsNone')}
            options={(vendors.data?.data ?? []).map((vendor) => ({ value: vendor.id, label: vendor.display_name }))}
            onChange={setExtendsVendor}
          />
        </Field>
        <Field label={t('compat:fieldHosts')} htmlFor="new-profile-hosts" hint={t('compat:fieldHostsHint')}>
          <TagInput id="new-profile-hosts" value={hosts} onChange={setHosts} placeholder="relay.example.com" />
        </Field>
      </div>
    </Drawer>
  )
}
