import type { MetadataDraft } from './model-config'
import type { AXIS_LABEL, INDEPENDENT_AXES } from './types'

export type PricingAxis = keyof typeof AXIS_LABEL | typeof INDEPENDENT_AXES[number]
type Modality = 'image' | 'audio'
type Direction = 'input' | 'output'

const RULES: Partial<Record<PricingAxis, { cache?: boolean; modality?: Modality; direction?: Direction }>> = {
  cache_ratio: { cache: true },
  cache_write_ratio: { cache: true },
  cache_write_5m: { cache: true },
  cache_write_1h: { cache: true },
  image_ratio: { modality: 'image', direction: 'input' },
  audio_ratio: { modality: 'audio', direction: 'input' },
  audio_completion_ratio: { modality: 'audio', direction: 'output' },
  image_output: { modality: 'image', direction: 'output' },
  image_cache_read: { cache: true, modality: 'image', direction: 'input' },
  image_cache_write: { cache: true, modality: 'image', direction: 'input' },
  audio_cache_read: { cache: true, modality: 'audio', direction: 'input' },
  audio_cache_write: { cache: true, modality: 'audio', direction: 'input' },
}

const MODALITY_REASONS = {
  input: { image: 'admin:modelMeta.noImageInput', audio: 'admin:modelMeta.noAudioInput' },
  output: { image: 'admin:modelMeta.noImageOutput', audio: 'admin:modelMeta.noAudioOutput' },
} as const

/** Editor guard only: never infer support from an ID/vendor, or change saved billing rates.
 * Empty modality lists and omitted capability flags mean unknown, not unsupported.
 * Model kinds are not a modality whitelist: chat/embedding can be multimodal.
 */
export function pricingDisabledReason(metadata: MetadataDraft, axis: PricingAxis) {
  const rule = RULES[axis]
  if (!rule) return undefined
  if (rule.cache && metadata.capabilities.prompt_cache === false) return 'admin:modelMeta.noPromptCache' as const
  if (!rule.modality || !rule.direction) return undefined
  const modalities = metadata[rule.direction === 'input' ? 'input_modalities' : 'output_modalities']
  const capability = rule.modality === 'audio' ? 'audio' : rule.direction === 'input' ? 'vision' : undefined
  if ((modalities.length > 0 && !modalities.includes(rule.modality))
    || (capability && metadata.capabilities[capability] === false)) {
    return MODALITY_REASONS[rule.direction][rule.modality]
  }
  return undefined
}
