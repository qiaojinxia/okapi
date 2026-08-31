import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelRow, ChannelSettings } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { KeyParamRow } from '@/features/channels/KeyParamRow'
import { ModelPicker } from '@/features/channels/ModelPicker'
import { PROVIDERS, readSettings } from '@/features/channels/types'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { TagInput } from '@/components/ui/tag-input'
import { PoolMembership } from '@/features/channels/PoolMembership'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 渠道抽屉：新建与编辑共用。
///
/// 字段按语义分段（接入 / 模型 / 调度 / 请求与计费行为 / 可见性），而不是把十几个
/// 输入框平铺——用户需要知道哪些字段该一起考虑。
///
/// 凭证轮换、模型发现、per-key 参数、可见性各自是独立端点（审计语义不同），
/// 故它们在段内单独提交，不并进主"保存"。
export function ChannelDrawer({
  channel,
  onClose,
  onDone,
}: {
  channel: ChannelRow | undefined
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const isEdit = channel !== undefined
  const [msg, setMsg] = useState<string | null>(null)
  const [form, setForm] = useState({
    name: channel?.name ?? '',
    provider: channel?.provider ?? 'openai',
    api_base: channel?.api_base ?? '',
    priority: String(channel?.priority ?? 0),
  })
  const [models, setModels] = useState<string[]>(channel?.models ?? [])
  const [credential, setCredential] = useState('')
  const [settings, setSettings] = useState<ChannelSettings>(readSettings(channel?.settings ?? null))

  const create = useMutation({
    mutationFn: () =>
      apiFetch('/admin/channels', {
        method: 'POST',
        body: {
          name: form.name,
          provider: form.provider,
          api_base: form.api_base,
          credential,
          models,
          priority: Number(form.priority) || 0,
          settings,
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channel?.id ?? 0}`, {
        method: 'PATCH',
        body: {
          name: form.name,
          api_base: form.api_base,
          models,
          priority: Number(form.priority) || 0,
          settings,
        },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      onDone()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const rotate = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channel?.id ?? 0}/credential`, {
        method: 'POST',
        body: { credential },
      }),
    onSuccess: () => {
      setCredential('')
      setMsg(t('admin:credentialRotated'))
      onDone()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const canSubmit = isEdit
    ? form.name.trim() !== ''
    : form.name.trim() !== '' && credential.trim() !== '' && models.length > 0

  return (
    <Drawer
      open
      onClose={onClose}
      title={isEdit ? t('admin:editChannel', { name: channel.name }) : t('admin:createChannel')}
      description={t('admin:channelDrawerDesc')}
      footer={
        <>
          {msg !== null && <span className="mr-auto text-xs text-muted-foreground">{msg}</span>}
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={!canSubmit || create.isPending || save.isPending}
            onClick={() => (isEdit ? save.mutate() : create.mutate())}
          >
            {isEdit ? t('common:save') : t('common:create')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('admin:groupBasic')} hint={t('admin:groupBasicHint')}>
        <div className="grid grid-cols-2 gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="d-name">{t('admin:channelName')}</Label>
            <Input
              id="d-name"
              value={form.name}
              onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="d-provider">{t('admin:provider')}</Label>
            {isEdit ? (
              // 协议决定请求转换路径，改了等于换渠道语义；已有渠道只读，需要换就新建
              <Input id="d-provider" value={form.provider} readOnly className="opacity-60" />
            ) : (
              <Select
                id="d-provider"
                value={form.provider}
                onChange={(v) => setForm((f) => ({ ...f, provider: v }))}
                options={PROVIDERS.map((p) => ({ value: p, label: p }))}
              />
            )}
          </div>
          <div className="col-span-2 flex flex-col gap-1.5">
            <Label htmlFor="d-base">{t('admin:apiBase')}</Label>
            <Input
              id="d-base"
              value={form.api_base}
              placeholder="https://api.openai.com/v1"
              onChange={(e) => setForm((f) => ({ ...f, api_base: e.target.value }))}
            />
          </div>
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:groupCredential')} hint={t('admin:groupCredentialHint')}>
        <div className="flex items-end gap-2">
          <div className="flex flex-1 flex-col gap-1.5">
            <Label htmlFor="d-cred">
              {isEdit ? t('admin:rotateCredential') : t('admin:credential')}
            </Label>
            <Input
              id="d-cred"
              value={credential}
              placeholder="sk-..."
              onChange={(e) => setCredential(e.target.value)}
            />
          </div>
          {isEdit && (
            <Button
              variant="outline"
              disabled={credential.trim() === '' || rotate.isPending}
              onClick={() => rotate.mutate()}
            >
              {t('admin:rotate')}
            </Button>
          )}
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:groupModels')} hint={t('admin:groupModelsHint')}>
        <ModelPicker value={models} onChange={setModels} />
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="d-models">{t('admin:modelsManual')}</Label>
          <TagInput
            id="d-models"
            value={models}
            onChange={setModels}
            placeholder={t('admin:tagInputHint')}
          />
        </div>
        {isEdit && (
          <Button
            size="sm"
            variant="outline"
            className="self-start"
            onClick={() =>
              void queryClient
                .fetchQuery({
                  queryKey: qk.channelModels(channel.id),
                  queryFn: () =>
                    apiFetch<{ data: string[] }>(`/admin/channels/${channel.id}/fetch-models`),
                })
                .then((r) => {
                  setModels(r.data)
                  setMsg(t('admin:discovered', { n: r.data.length }))
                })
                .catch((err: unknown) => setMsg(describeError(err)))
            }
          >
            {t('admin:fetchModels')}
          </Button>
        )}
      </FieldGroup>

      <FieldGroup title={t('admin:groupSchedule')} hint={t('admin:groupScheduleHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="d-priority">{t('admin:priority')}</Label>
          <Input
            id="d-priority"
            className="w-24"
            inputMode="numeric"
            value={form.priority}
            onChange={(e) => setForm((f) => ({ ...f, priority: e.target.value }))}
          />
        </div>
        {isEdit && (channel.keys ?? []).length > 0 && (
          <div className="flex flex-col gap-2">
            <Label>{t('admin:channelKeys')}</Label>
            <p className="text-xs text-muted-foreground">{t('admin:channelKeysHint')}</p>
            {channel.keys.map((k) => (
              <KeyParamRow
                key={k.id}
                channelId={channel.id}
                row={k}
                onDone={onDone}
                onMsg={setMsg}
              />
            ))}
          </div>
        )}
      </FieldGroup>

      <FieldGroup title={t('admin:groupBehavior')} hint={t('admin:groupBehaviorHint')}>
        <Switch
          label={t('admin:thinkingToContent')}
          description={t('admin:thinkingToContentHint')}
          checked={settings.thinking_to_content}
          onChange={(v) => setSettings((s) => ({ ...s, thinking_to_content: v }))}
        />
        <Switch
          label={t('admin:billByResponseModel')}
          description={t('admin:billByResponseModelHint')}
          checked={settings.bill_by_response_model}
          onChange={(v) => setSettings((s) => ({ ...s, bill_by_response_model: v }))}
        />
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="d-strip">{t('admin:stripFields')}</Label>
          <p className="text-xs text-muted-foreground">{t('admin:stripFieldsHint')}</p>
          <TagInput
            id="d-strip"
            value={settings.strip_request_fields}
            onChange={(v) => setSettings((s) => ({ ...s, strip_request_fields: v }))}
            placeholder="logit_bias"
          />
        </div>
      </FieldGroup>

      {isEdit && (
        <PoolMembership
          channelId={channel.id}
          current={channel.pools}
          onDone={onDone}
          onMsg={setMsg}
        />
      )}
    </Drawer>
  )
}
