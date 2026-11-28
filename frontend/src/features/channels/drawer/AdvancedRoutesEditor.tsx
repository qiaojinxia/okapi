import { Plus, Route, Trash2 } from 'lucide-react'
import { useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AdvancedRoute } from '@/lib/contracts.gen'
import { ADVANCED_ROUTE_CONVERTERS } from '@/lib/contracts.gen'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { TagInput } from '@/components/ui/tag-input'

const INCOMING_PATH_OPTIONS = [
  { value: '/v1/chat/completions', label: '/v1/chat/completions (OpenAI Chat)' },
  { value: '/v1/messages', label: '/v1/messages (Anthropic Messages)' },
  { value: '/v1/responses', label: '/v1/responses (OpenAI Responses)' },
  { value: '/v1/responses/compact', label: '/v1/responses/compact (Responses Compact)' },
  { value: '/v1beta/models/{model}:generateContent', label: '/v1beta/models/{model}:generateContent (Gemini)' },
  { value: '*', label: '* (所有对话入口)' },
] as const

type AuthMode = 'default' | 'header' | 'query'

interface RouteRow {
  id: number
  incoming_path: string
  upstream_path: string
  stream_upstream_path: string
  converter: string
  models: string[]
  authMode: AuthMode
  authHeader: string
  authPrefix: string
  authQueryParam: string
  pass_through_body: boolean
}

function toRow(id: number, r: AdvancedRoute): RouteRow {
  const authMode: AuthMode = r.auth?.query_param
    ? 'query'
    : r.auth?.header || r.auth?.prefix !== undefined
      ? 'header'
      : 'default'
  return {
    id,
    incoming_path: r.incoming_path || '/v1/chat/completions',
    upstream_path: r.upstream_path || '',
    stream_upstream_path: r.stream_upstream_path ?? '',
    converter: r.converter ?? 'auto',
    models: r.models ?? [],
    authMode,
    authHeader: r.auth?.header ?? '',
    authPrefix: r.auth?.prefix ?? '',
    authQueryParam: r.auth?.query_param ?? '',
    pass_through_body: r.pass_through_body ?? false,
  }
}

function toDto(row: RouteRow): AdvancedRoute | null {
  const incoming = row.incoming_path.trim()
  const upstream = row.upstream_path.trim()
  if (!incoming || !upstream) return null
  const dto: AdvancedRoute = {
    incoming_path: incoming,
    upstream_path: upstream,
  }
  const streamPath = row.stream_upstream_path.trim()
  if (streamPath) dto.stream_upstream_path = streamPath
  const conv = row.converter.trim()
  if (conv && conv !== 'auto') dto.converter = conv
  if (row.models.length > 0) dto.models = row.models
  if (row.authMode === 'header') {
    const header = row.authHeader.trim()
    const prefix = row.authPrefix
    if (header || prefix !== '') {
      dto.auth = {
        ...(header ? { header } : {}),
        ...(prefix !== '' ? { prefix } : {}),
      }
    }
  } else if (row.authMode === 'query') {
    const qp = row.authQueryParam.trim()
    if (qp) dto.auth = { query_param: qp }
  }
  if (row.pass_through_body) dto.pass_through_body = true
  return dto
}

function safeUpstreamPath(raw: string): boolean {
  const s = raw.trim()
  return (
    s.startsWith('/') &&
    !s.startsWith('//') &&
    !s.includes('..') &&
    !s.includes('://') &&
    !s.includes('@') &&
    !/\s/.test(s)
  )
}

/// 声明式路由映射编辑器（`settings.advanced_routes`）：
/// 通过渠道 JSON 直接配置非标准上游路径、自定义鉴权头/查询参数与注册表协议转换器。
export function AdvancedRoutesEditor({
  value,
  onChange,
}: {
  value: AdvancedRoute[]
  onChange: (next: AdvancedRoute[]) => void
}) {
  const { t } = useTranslation()
  const seq = useRef(0)
  const [rows, setRows] = useState<RouteRow[]>(() =>
    value.map((route) => toRow(seq.current++, route)),
  )

  const converterOptions = useMemo(
    () =>
      ADVANCED_ROUTE_CONVERTERS.map((name) => ({
        value: name,
        label: name === 'auto' ? t('admin:advancedRoutesConverterAuto') : name,
      })),
    [t],
  )

  const authModeOptions = useMemo(
    () => [
      { value: 'default', label: t('admin:advancedRoutesAuthDefault') },
      { value: 'header', label: t('admin:advancedRoutesAuthHeader') },
      { value: 'query', label: t('admin:advancedRoutesAuthQuery') },
    ],
    [t],
  )

  const commit = (next: RouteRow[]) => {
    setRows(next)
    const serialized = next.map(toDto).filter((item): item is AdvancedRoute => item !== null)
    onChange(serialized)
  }

  const updateRow = (id: number, patch: Partial<RouteRow>) =>
    commit(rows.map((row) => (row.id === id ? { ...row, ...patch } : row)))

  const removeRow = (id: number) => commit(rows.filter((row) => row.id !== id))

  const addPreset = (preset: AdvancedRoute) =>
    commit([...rows, toRow(seq.current++, preset)])

  return (
    <div data-slot="advanced-routes-editor" className="flex flex-col gap-3 border-t border-border/60 pt-4">
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-1.5">
          <Route aria-hidden className="h-3.5 w-3.5 text-muted-foreground" />
          <Label className="text-[13px] text-foreground">{t('admin:advancedRoutes')}</Label>
        </div>
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:advancedRoutesHint')}</p>
      </div>

      {rows.map((row, idx) => {
        const upstreamBad = row.upstream_path.trim() !== '' && !safeUpstreamPath(row.upstream_path)
        const streamBad =
          row.stream_upstream_path.trim() !== '' && !safeUpstreamPath(row.stream_upstream_path)
        return (
          <div
            key={row.id}
            data-slot="advanced-route-row"
            className="flex flex-col gap-3 rounded-lg border border-border/70 bg-card/75 p-3"
          >
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs font-medium text-muted-foreground">
                #{idx + 1} · {row.incoming_path}
              </span>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className="h-7 w-7 text-muted-foreground hover:text-destructive"
                aria-label={t('common:delete')}
                onClick={() => removeRow(row.id)}
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </div>

            <div className="grid gap-2.5 sm:grid-cols-2">
              <div className="flex flex-col gap-1">
                <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesIncoming')}</Label>
                <Select
                  value={row.incoming_path}
                  onChange={(incoming_path) => updateRow(row.id, { incoming_path })}
                  options={[...INCOMING_PATH_OPTIONS]}
                />
              </div>

              <div className="flex flex-col gap-1">
                <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesConverter')}</Label>
                <Select
                  value={row.converter}
                  onChange={(converter) =>
                    updateRow(row.id, {
                      converter,
                      pass_through_body:
                        converter === 'auto' || converter === 'passthrough' || converter.endsWith('_passthrough')
                          ? row.pass_through_body
                          : false,
                    })
                  }
                  options={converterOptions}
                />
              </div>

              <div className="flex flex-col gap-1">
                <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesUpstream')}</Label>
                <Input
                  className="h-8 font-mono text-xs"
                  value={row.upstream_path}
                  aria-invalid={upstreamBad}
                  placeholder="/api/v2/chat/completions"
                  onChange={(e) => updateRow(row.id, { upstream_path: e.target.value })}
                />
              </div>

              <div className="flex flex-col gap-1">
                <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesStreamUpstream')}</Label>
                <Input
                  className="h-8 font-mono text-xs"
                  value={row.stream_upstream_path}
                  aria-invalid={streamBad}
                  placeholder="/v1/models/{model}:streamGenerateContent?alt=sse"
                  onChange={(e) => updateRow(row.id, { stream_upstream_path: e.target.value })}
                />
              </div>
            </div>

            <div className="flex flex-col gap-1">
              <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesModels')}</Label>
              <TagInput
                value={row.models}
                onChange={(models) => updateRow(row.id, { models })}
                placeholder="claude-*, deepseek-*"
              />
            </div>

            <div className="grid gap-2.5 sm:grid-cols-3">
              <div className="flex flex-col gap-1">
                <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesAuth')}</Label>
                <Select
                  value={row.authMode}
                  onChange={(mode) => updateRow(row.id, { authMode: mode as AuthMode })}
                  options={authModeOptions}
                />
              </div>

              {row.authMode === 'header' && (
                <>
                  <div className="flex flex-col gap-1">
                    <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesAuthHeaderName')}</Label>
                    <Input
                      className="h-8 font-mono text-xs"
                      value={row.authHeader}
                      placeholder="x-api-key"
                      onChange={(e) => updateRow(row.id, { authHeader: e.target.value })}
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesAuthPrefix')}</Label>
                    <Input
                      className="h-8 font-mono text-xs"
                      value={row.authPrefix}
                      placeholder="Bearer "
                      onChange={(e) => updateRow(row.id, { authPrefix: e.target.value })}
                    />
                  </div>
                </>
              )}

              {row.authMode === 'query' && (
                <div className="flex flex-col gap-1 sm:col-span-2">
                  <Label className="text-[11px] text-muted-foreground">{t('admin:advancedRoutesAuthQueryParam')}</Label>
                  <Input
                    className="h-8 font-mono text-xs"
                    value={row.authQueryParam}
                    placeholder="key"
                    onChange={(e) => updateRow(row.id, { authQueryParam: e.target.value })}
                  />
                </div>
              )}
            </div>

            {(row.converter === 'auto' ||
              row.converter === 'passthrough' ||
              row.converter.endsWith('_passthrough')) && (
              <Switch
                label={t('admin:advancedRoutesPassThroughBody')}
                checked={row.pass_through_body}
                onChange={(pass_through_body) => updateRow(row.id, { pass_through_body })}
              />
            )}
          </div>
        )
      })}

      <div className="flex flex-wrap items-center gap-2">
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() =>
            addPreset({
              incoming_path: '/v1/chat/completions',
              upstream_path: '/chat/completions',
              converter: 'auto',
            })
          }
        >
          <Plus className="h-3.5 w-3.5" />
          {t('admin:advancedRoutesAdd')}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="text-xs text-muted-foreground"
          onClick={() =>
            addPreset({
              incoming_path: '/v1/chat/completions',
              upstream_path: '/v1/messages',
              converter: 'openai_to_anthropic',
              auth: { header: 'x-api-key', prefix: '' },
            })
          }
        >
          + OpenAI → Anthropic
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="text-xs text-muted-foreground"
          onClick={() =>
            addPreset({
              incoming_path: '/v1beta/models/{model}:generateContent',
              upstream_path: '/models/{model}:generateContent',
              stream_upstream_path: '/models/{model}:streamGenerateContent?alt=sse',
              converter: 'passthrough',
            })
          }
        >
          + Gemini {`{model}`}
        </Button>
      </div>
    </div>
  )
}
