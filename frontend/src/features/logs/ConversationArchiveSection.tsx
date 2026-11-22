import { useQuery } from '@tanstack/react-query'
import { Bot, Brain, Check, Code2, Copy, Database, MessageSquareText, Wrench } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { DetailSection } from '@/features/logs/detail-ui'
import {
  type ArchiveRecord,
  fetchAdminConversation,
  fetchPortalArchivePolicy,
  fetchPortalConversation,
  parseRequestMessages,
  parseToolCalls,
  prettyJson,
} from '@/features/logs/conversation-api'
import { ApiError } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

/// 内嵌于管理员/门户日志详情抽屉中的「对话存档」区块。
/// 支持对话气泡视图与原始 JSON 视图切换，自动解析多协议消息、思考过程与工具调用。
export function ConversationArchiveSection({
  mode,
  requestId,
}: {
  mode: 'admin' | 'portal'
  requestId: string
}) {
  const { t } = useTranslation()

  const portalPolicy = useQuery({
    queryKey: qk.portalConversationPolicy,
    queryFn: fetchPortalArchivePolicy,
    enabled: mode === 'portal',
    staleTime: 60_000,
  })

  const canFetch = mode === 'admin' || portalPolicy.data?.allow_user_view === true

  const query = useQuery({
    queryKey: [mode, 'conversation-archive', requestId],
    queryFn: () =>
      mode === 'admin' ? fetchAdminConversation(requestId) : fetchPortalConversation(requestId),
    enabled: canFetch && requestId !== '',
    retry: false,
  })

  if (mode === 'portal' && portalPolicy.data?.allow_user_view !== true) {
    return null
  }

  if (query.isPending) {
    return (
      <DetailSection icon={MessageSquareText} title={t('logs:conversationTitle')}>
        <div className="h-16 animate-pulse rounded-lg bg-muted/50" />
      </DetailSection>
    )
  }

  if (query.isError) {
    const err = query.error
    if (err instanceof ApiError && (err.status === 404 || err.status === 403)) {
      if (err.status === 403) return null
      return (
        <DetailSection icon={MessageSquareText} title={t('logs:conversationTitle')}>
          <p className="text-xs text-muted-foreground">{t('logs:conversationNotArchived')}</p>
        </DetailSection>
      )
    }
    return null
  }

  return (
    <DetailSection icon={MessageSquareText} title={t('logs:conversationTitle')}>
      <ConversationViewer record={query.data.data} />
    </DetailSection>
  )
}

export function ConversationViewer({ record }: { record: ArchiveRecord }) {
  const { t } = useTranslation()
  const [view, setView] = useState<'chat' | 'raw'>('chat')
  const [copied, setCopied] = useState(false)

  const turns = parseRequestMessages(record.request_body)
  const toolCalls = parseToolCalls(record.tool_calls)

  const copyPayload = () => {
    const text =
      view === 'raw'
        ? JSON.stringify(
            {
              request: safeParse(record.request_body),
              response: safeParse(record.response_body),
            },
            null,
            2,
          )
        : [
            ...turns.map((turn) => `[${turn.role}]\n${turn.text}`),
            record.reasoning_text ? `[reasoning]\n${record.reasoning_text}` : '',
            record.response_text ? `[assistant]\n${record.response_text}` : '',
          ]
            .filter(Boolean)
            .join('\n\n')
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }

  return (
    <div data-slot="conversation-viewer" className="flex flex-col gap-3">
      {/* 顶部工具栏：后端标记、截断告警、视图切换、复制 */}
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge variant="muted" className="font-mono text-[11px]">
            <Database className="h-3 w-3" />
            {record.storage === 'clickhouse' ? 'ClickHouse' : 'PostgreSQL'}
          </Badge>
          {record.truncated && (
            <Badge variant="warning" dot>
              {t('logs:conversationTruncated')}
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-1.5">
          <div className="inline-flex rounded-lg border border-border/80 bg-muted/40 p-0.5 text-xs">
            <button
              type="button"
              onClick={() => setView('chat')}
              className={cn(
                'inline-flex cursor-pointer items-center gap-1 rounded-md px-2 py-1 font-medium transition-colors',
                view === 'chat'
                  ? 'bg-card text-foreground shadow-xs'
                  : 'text-muted-foreground hover:text-foreground',
              )}
            >
              <MessageSquareText className="h-3 w-3" />
              {t('logs:conversationChatView')}
            </button>
            <button
              type="button"
              onClick={() => setView('raw')}
              className={cn(
                'inline-flex cursor-pointer items-center gap-1 rounded-md px-2 py-1 font-medium transition-colors',
                view === 'raw'
                  ? 'bg-card text-foreground shadow-xs'
                  : 'text-muted-foreground hover:text-foreground',
              )}
            >
              <Code2 className="h-3 w-3" />
              {t('logs:conversationRawView')}
            </button>
          </div>
          <Button size="sm" variant="outline" className="h-7 px-2 text-xs" onClick={copyPayload}>
            {copied ? <Check className="h-3 w-3 text-success" /> : <Copy className="h-3 w-3" />}
            {copied ? t('common:copied') : t('common:copy')}
          </Button>
        </div>
      </div>

      {view === 'chat' ? (
        <div className="flex flex-col gap-2.5">
          {turns.length === 0 ? (
            <pre className="max-h-64 overflow-auto rounded-lg border border-border/75 bg-muted/25 p-3 font-mono text-xs leading-5 whitespace-pre-wrap break-words">
              {prettyJson(record.request_body) || '—'}
            </pre>
          ) : (
            turns.map((turn, idx) => (
              <div
                key={`${turn.role}-${idx}`}
                data-role={turn.role}
                className={cn(
                  'flex flex-col gap-1 rounded-xl border p-3 text-xs leading-5',
                  turn.role === 'system'
                    ? 'border-border/75 bg-muted/35'
                    : turn.role === 'user'
                      ? 'border-primary/25 bg-primary/[0.04]'
                      : turn.role === 'tool'
                        ? 'border-info/25 bg-info/[0.05]'
                        : 'border-border/80 bg-card',
                )}
              >
                <div className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                  <span>{t(`logs:role_${turn.role}`, { defaultValue: turn.role })}</span>
                  {turn.toolName && <span className="font-mono normal-case">· {turn.toolName}</span>}
                </div>
                <div className="max-h-72 overflow-y-auto whitespace-pre-wrap break-words text-[13px] leading-6 text-foreground">
                  {turn.text}
                </div>
              </div>
            ))
          )}

          {/* 助手回复（含思考过程与工具调用） */}
          {(record.reasoning_text !== '' ||
            record.response_text !== '' ||
            toolCalls.length > 0) && (
            <div
              data-slot="conversation-assistant-reply"
              className="flex flex-col gap-2.5 rounded-xl border border-success/30 bg-success/[0.04] p-3"
            >
              <div className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-success">
                <Bot className="h-3.5 w-3.5" />
                <span>{t('logs:conversationReply')}</span>
              </div>

              {record.reasoning_text !== '' && (
                <details className="rounded-lg border border-border/75 bg-card/80 px-3 py-2 text-xs">
                  <summary className="flex cursor-pointer items-center gap-1.5 font-medium text-muted-foreground select-none">
                    <Brain className="h-3.5 w-3.5 text-primary" />
                    {t('logs:conversationReasoning')}
                  </summary>
                  <div className="mt-2 max-h-64 overflow-y-auto whitespace-pre-wrap break-words border-t border-border/50 pt-2 leading-5 text-muted-foreground">
                    {record.reasoning_text}
                  </div>
                </details>
              )}

              {toolCalls.length > 0 && (
                <div className="flex flex-col gap-1.5">
                  {toolCalls.map((call, idx) => (
                    <div
                      key={`${call.id}-${idx}`}
                      className="rounded-lg border border-border/75 bg-card p-2.5 text-xs"
                    >
                      <div className="mb-1 flex items-center gap-1.5 font-mono font-semibold text-foreground">
                        <Wrench className="h-3 w-3 text-primary" />
                        <span>{call.name || 'tool_call'}</span>
                        {call.id && <span className="font-normal text-muted-foreground">({call.id})</span>}
                      </div>
                      <pre className="max-h-48 overflow-auto font-mono text-[11.5px] leading-5 whitespace-pre-wrap break-all text-muted-foreground">
                        {prettyJson(call.arguments)}
                      </pre>
                    </div>
                  ))}
                </div>
              )}

              {record.response_text !== '' && (
                <div className="max-h-96 overflow-y-auto whitespace-pre-wrap break-words text-[13px] leading-6 text-foreground">
                  {record.response_text}
                </div>
              )}
            </div>
          )}
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1">
            <span className="text-xs font-semibold text-muted-foreground">
              {t('logs:conversationRequestJson')}
            </span>
            <pre className="max-h-72 overflow-auto rounded-lg border border-border/80 bg-muted/25 p-3 font-mono text-[11.5px] leading-5 whitespace-pre-wrap break-all">
              {prettyJson(record.request_body) || '—'}
            </pre>
          </div>
          <div className="flex flex-col gap-1">
            <span className="text-xs font-semibold text-muted-foreground">
              {t('logs:conversationResponseJson')}
            </span>
            <pre className="max-h-72 overflow-auto rounded-lg border border-border/80 bg-muted/25 p-3 font-mono text-[11.5px] leading-5 whitespace-pre-wrap break-all">
              {prettyJson(record.response_body) || '—'}
            </pre>
          </div>
        </div>
      )}
    </div>
  )
}

function safeParse(raw: string): unknown {
  try {
    return JSON.parse(raw)
  } catch {
    return raw
  }
}
