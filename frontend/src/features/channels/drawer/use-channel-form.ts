import { useState } from 'react'
import type { ChannelRow, ChannelSettings } from '@/features/channels/types'
import { toBinding } from '@/features/proxies/EgressPicker'
import type { EgressDraft } from '@/features/proxies/EgressPicker'
import { parseLimit } from '../account-controls/policy'
import { useAccountCapabilities } from '../account-controls/api'
import { costMilliToRatio, ratioToCostMilli, readSettings } from '@/features/channels/types'
import { defaultMembership } from '@/features/channels/PoolMembership'
import type { PoolMember } from '@/features/pools/types'
import { parseInt32 } from '@/lib/int32'
import { usePermission } from '@/hooks/use-auth'

/// 渠道抽屉的表单状态与由它算出的校验结果：新建与编辑共用一份，四个分区组件各取所需。
export function useChannelForm(channel: ChannelRow | undefined) {
  const isEdit = channel !== undefined
  // 入池只有全站范围的 channel.write 能做（后端 pools_require_all_scope）；own 范围建出来是孤儿，等管理员审过再入池
  const canManagePools = usePermission()('channel.write')
  const [form, setForm] = useState({
    name: channel?.name ?? '',
    provider: channel?.provider ?? 'openai',
    api_base: channel?.api_base ?? '',
    priority: String(channel?.priority ?? 0),
    cost: costMilliToRatio(channel?.cost_milli ?? 1000),
  })
  const account = useAccountCapabilities(form.provider)
  const authorization = account.capabilities?.authorization
  const accountReady = account.isSuccess
  const costMilli = ratioToCostMilli(form.cost)
  const [models, setModels] = useState<string[]>(channel?.models ?? [])
  const [credential, setCredential] = useState('')
  const [oauthInput, setOauthInput] = useState<'login' | 'import'>(
    channel?.keys.some((key) => key.oauth_refreshable === false)
      ? 'import' : 'login',
  )
  const [rotationKeyId, setRotationKeyId] = useState<number | undefined>(
    channel?.keys.length === 1 ? channel.keys[0]?.id : undefined,
  )
  const [settings, setSettings] = useState<ChannelSettings>(readSettings(channel?.settings ?? null))
  const [controlValid, setControlValid] = useState(true)
  const [newConcurrency, setNewConcurrency] = useState('')
  const concurrency = parseLimit(newConcurrency)
  const concurrencyValid = concurrency !== null && (concurrency === undefined || concurrency <= 2_147_483_647)
  // 留空按 0；写错（含越界）就拦下提交，不再悄悄按 0 存
  const priority = parseInt32(form.priority)
  // 新建时的池成员关系：缺省只进 default 池（建完即对 default 分组可用）；
  // 渠道只服务它所在的池，全站分组都配了专属池的站点在这里勾对应的池
  const [newPools, setNewPools] = useState<PoolMember[]>(defaultMembership)
  // 新建时的出口（§11.41）：缺省继承全局默认；OAuth 登录在换码前就按它选代理
  const [newEgress, setNewEgress] = useState<EgressDraft>({ mode: 'inherit' })
  const egressBinding = toBinding(newEgress)

  // Login uses exchange; imported Claude tokens use the ordinary channel create endpoint.
  const oauth = Boolean(authorization)
  const importToken = Boolean(authorization?.access_token_prefix) && oauthInput === 'import'
  // 缺省地址以后端协议注册表为准（/admin/channels/providers 的 default_base）。注册表明说没有缺省地址的协议才必填；
  // 目录还没到、或不认识这个协议时前端不判必填——新建本来就要等目录就绪，保存时后端照样校验
  const defaultBase = account.descriptor?.default_base ?? undefined
  const endpointRequired = account.descriptor !== undefined && !account.descriptor.default_base
  const endpointValid = !endpointRequired || form.api_base.trim() !== ''
  // 拉上游模型：已保存的渠道走渠道自己的凭证；新建时用表单里刚填的（订阅账号要先登录，没有草稿凭证可用）
  const canDiscover = isEdit || (accountReady && !oauth && endpointValid && credential.trim() !== '' && egressBinding !== null)
  const [unknownModels, setUnknownModels] = useState<string[]>([])
  return {
    isEdit,
    canManagePools,
    form,
    setForm,
    account,
    authorization,
    accountReady,
    costMilli,
    models,
    setModels,
    credential,
    setCredential,
    oauthInput,
    setOauthInput,
    rotationKeyId,
    setRotationKeyId,
    settings,
    setSettings,
    controlValid,
    setControlValid,
    newConcurrency,
    setNewConcurrency,
    concurrency,
    concurrencyValid,
    priority,
    newPools,
    setNewPools,
    newEgress,
    setNewEgress,
    egressBinding,
    oauth,
    importToken,
    defaultBase,
    endpointRequired,
    endpointValid,
    canDiscover,
    unknownModels,
    setUnknownModels,
  }
}

export type ChannelForm = ReturnType<typeof useChannelForm>
