import type { TFunction } from 'i18next'
import type { FlowNode } from './types'

const ENTITY_STAGES = ['user', 'api_key', 'channel']
export function flowIdentity(node: FlowNode, t: TFunction) {
  const entity = ENTITY_STAGES.includes(node.stage)
  const raw = node.label?.trim()
  const name = raw && (node.entity_status === 'active' || node.entity_status === 'deleted' || (raw !== `#${node.key}` && raw !== node.key)) ? raw : undefined
  const legacyMissing = entity && (!raw || raw === `#${node.key}` || raw === node.key) && node.entity_status == null
  const missing = node.entity_status === 'missing' || legacyMissing
  let primary: string
  if (node.other) primary = t('analytics:other')
  else if (!node.key) primary = t(entity ? 'flow:unassigned' : 'analysis:notCollected')
  else if (entity && (node.key === '0' || node.entity_status === 'unassigned')) primary = t('flow:unassigned')
  else if (entity) primary = name ?? (raw && !/^#?\d+$/.test(raw) ? raw : t(`flow:${missing ? 'historical' : 'unnamed'}_${node.stage}`))
  else if (node.stage === 'group' && node.key === 'default') primary = t('flow:defaultGroup')
  else primary = raw || node.key
  const id = entity && node.key !== '0' && !node.other ? `#${node.key}` : ''
  const context = [node.owner_name, node.key_prefix ? `${node.key_prefix}…` : null, node.provider].filter(Boolean).join(' · ')
  const status = node.entity_status === 'deleted' ? t('flow:deleted') : missing ? t('flow:missing') : ''
  const detail = [id, context, status].filter(Boolean).join(' · ')
  return { primary, detail, id, missing, deleted: node.entity_status === 'deleted' }
}

// Approximate rendered width instead of truncating CJK and Latin labels at the same character count.
export function flowShortName(value: string, width = 124): string {
  let used = 0, result = ''
  for (const c of value) {
    used += c.charCodeAt(0) <= 127 ? 6 : 11
    if (used > width - 11) return `${result}…`
    result += c
  }
  return value
}
