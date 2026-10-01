import { CAPABILITY_KEYS, type ModelListRow } from './types'

export interface MetadataDraft {
  display_name: string
  vendor: string
  description: string
  kind: string
  input_modalities: string[]
  output_modalities: string[]
  capabilities: Record<string, boolean>
  context_window: string
  max_output: string
}

export function metadataDraft(model?: ModelListRow): MetadataDraft {
  return {
    display_name: model?.display_name ?? '', vendor: model?.vendor ?? '',
    description: model?.catalog_config?.description ?? '', kind: model?.catalog_config?.kind ?? '',
    input_modalities: model?.catalog_config?.input_modalities ?? [],
    output_modalities: model?.catalog_config?.output_modalities ?? [],
    capabilities: Object.fromEntries(CAPABILITY_KEYS.filter((key) => typeof model?.capabilities?.[key] === 'boolean').map((key) => [key, model!.capabilities![key]])),
    context_window: String(model?.context_window ?? ''), max_output: String(model?.max_output ?? ''),
  }
}

export function validLimit(s: string) {
  return s.trim() === '' || (/^[1-9]\d*$/.test(s) && Number(s) <= 2_147_483_647)
}
export function validRatio(s: string) { return /^(?:0|[1-9]\d{0,5})(?:\.\d{1,6})?$/.test(s.trim()) }

// No floating point rounding: a dollar input must fit an exact safe micro-USD integer.
export function usdMicro(s: string): number | null {
  if (!/^(?:0|[1-9]\d*)(?:\.\d{1,6})?$/.test(s.trim())) return null
  const [whole, frac = ''] = s.trim().split('.')
  const micro = BigInt(whole) * 1_000_000n + BigInt(frac.padEnd(6, '0'))
  return micro <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(micro) : null
}

export function priceFromMicro(value: number | null | undefined): string {
  return value == null || !Number.isSafeInteger(value) || value < 0 ? '' : decimal(BigInt(value), 6)
}

const RATIO_SCALE = 1_000_000n
const PRICE_SCALE = 1_000_000_000_000_000_000n
const MAX_RATIO = 999_999_999_999n

function decimal(value: bigint, places: number): string {
  const s = value.toString().padStart(places + 1, '0')
  const fraction = s.slice(-places).replace(/0+$/, '')
  return `${s.slice(0, -places)}${fraction ? `.${fraction}` : ''}`
}

function priceScaled(raw: string): bigint | null {
  if (!/^(?:0|[1-9]\d{0,17})(?:\.\d{1,18})?$/.test(raw.trim())) return null
  const [whole, fraction = ''] = raw.trim().split('.')
  return BigInt(whole) * PRICE_SCALE + BigInt(fraction.padEnd(18, '0'))
}

function ratioScaled(raw: string): bigint | null {
  if (!validRatio(raw)) return null
  const [whole, fraction = ''] = raw.trim().split('.')
  return BigInt(whole) * RATIO_SCALE + BigInt(fraction.padEnd(6, '0'))
}

/** Exact display of existing rates: opening/saving a form must not round its prices. */
export function pricesFromRatios(baseMicro: number, model: string, completion: string) {
  const m = ratioScaled(model), c = ratioScaled(completion)
  if (!Number.isSafeInteger(baseMicro) || baseMicro <= 0 || m === null || c === null) return { input: '', output: '' }
  const base = BigInt(baseMicro)
  return { input: decimal(base * m * RATIO_SCALE, 18), output: decimal(base * m * c, 18) }
}

/** USD/1M is a UI view of the existing six-place ratios, not a new billing mode. */
export function ratiosFromPrices(baseMicro: number, input: string, output: string) {
  const i = priceScaled(input), o = priceScaled(output)
  if (!Number.isSafeInteger(baseMicro) || baseMicro <= 0 || i === null || o === null || (i === 0n && o > 0n)) return null
  const round = (n: bigint, d: bigint) => (n + d / 2n) / d
  const m = round(i, BigInt(baseMicro) * RATIO_SCALE)
  const c = i === 0n ? RATIO_SCALE : round(o * RATIO_SCALE, i)
  // A tiny paid lane must never silently become free after ratio rounding.
  if (m > MAX_RATIO || c > MAX_RATIO || (i > 0n && m === 0n) || (o > 0n && c === 0n)) return null
  const model_ratio = decimal(m, 6), completion_ratio = decimal(c, 6)
  const effective = pricesFromRatios(baseMicro, model_ratio, completion_ratio)
  return { model_ratio, completion_ratio, effective,
    approximate: priceScaled(effective.input) !== i || priceScaled(effective.output) !== o }
}

export function pricingAxes(model?: ModelListRow): Record<string, string> {
  const keys = ['model_ratio', 'completion_ratio', 'cache_ratio', 'cache_write_ratio',
    'audio_ratio', 'audio_completion_ratio', 'image_ratio'] as const
  return Object.fromEntries(keys.map((key) => [key, model?.[key] ?? '1']))
}

export function metadataPayload(m: MetadataDraft) {
  return {
    ...m, display_name: m.display_name.trim() || null, vendor: m.vendor.trim() || null,
    description: m.description.trim() || null, kind: m.kind || null,
    context_window: m.context_window.trim() === '' ? null : Number(m.context_window),
    max_output: m.max_output.trim() === '' ? null : Number(m.max_output),
  }
}
