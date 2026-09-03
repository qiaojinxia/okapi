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
import { Tabs } from '@/components/ui/tabs'
import { TagInput } from '@/components/ui/tag-input'
import {
  PoolMembership,
  PoolMembershipEditor,
  defaultMembership,
} from '@/features/channels/PoolMembership'
import type { PoolMember } from '@/features/pools/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

const EDIT_TABS = ['conn', 'models', 'sched', 'behavior'] as const
type EditTab = (typeof EDIT_TABS)[number]

/// 渠道抽屉：新建与编辑共用，但形态不同。
///
/// - 新建只问三件必答事（接入 / 凭证 / 模型），行为开关与调度参数走缺省——
///   建渠道时用户根本还不知道要不要 thinking 转正文，问了也是瞎选；
/// - 编辑按"接入 / 模型 / 调度 / 行为"分页签：此前六段纵排滚起来找不到北。
///   表单状态提升在抽屉层，切页签不丢改动；底部"保存"提交全部页签的字段。
///
/// 凭证轮换、模型发现、per-key 参数、可见性各自是独立端点（审计语义不同），
/// 故它们在所属页签内单独提交，不并进主"保存"。
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
  const [tab, setTab] = useState<EditTab>('conn')
  const [form, setForm] = useState({
    name: channel?.name ?? '',
    provider: channel?.provider ?? 'openai',
    api_base: channel?.api_base ?? '',
    priority: String(channel?.priority ?? 0),
  })
  const [models, setModels] = useState<string[]>(channel?.models ?? [])
  const [credential, setCredential] = useState('')
  const [settings, setSettings] = useState<ChannelSettings>(readSettings(channel?.settings ?? null))
  // 新建时的池成员关系：缺省只进 default 池（建完即对 default 分组可用）；
  // 渠道只服务它所在的池，全站分组都配了专属池的站点在这里勾对应的池
  const [newPools, setNewPools] = useState<PoolMember[]>(defaultMembership)

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
          pools: newPools,
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
      {isEdit && (
        <Tabs
          className="mb-4"
          items={EDIT_TABS.map((id) => ({
            id,
            label: t(
              (
                {
                  conn: 'admin:groupBasic',
                  models: 'admin:groupModels',
                  sched: 'admin:groupSchedule',
                  behavior: 'admin:groupBehavior',
                } as const
              )[id],
            ),
          }))}
          active={tab}
          onChange={(id) => setTab(id as EditTab)}
        />
      )}

      {(!isEdit || tab === 'conn') && (
        <>
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
        </>
      )}

      {(!isEdit || tab === 'models') && (
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
      )}

      {!isEdit && (
        <FieldGroup title={t('admin:poolMembership')} hint={t('admin:poolMembershipHint')}>
          <PoolMembershipEditor value={newPools} onChange={setNewPools} />
        </FieldGroup>
      )}

      {isEdit && tab === 'sched' && (
        <>
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
            {(channel.keys ?? []).length > 0 && (
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
          <PoolMembership
            channelId={channel.id}
            current={channel.pool_members ?? []}
            onDone={onDone}
            onMsg={setMsg}
          />
        </>
      )}

      {isEdit && tab === 'behavior' && (
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
      )}
    </Drawer>
  )
}
