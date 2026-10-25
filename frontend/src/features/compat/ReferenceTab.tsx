import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { useCompatVendors } from '@/features/channels/compat'
import { useCatalog } from '@/features/compat/api'
import { pretty } from '@/features/compat/diff'
import { recipeTitle } from '@/features/compat/RulesWorkbench'
import type { BalanceProbe, ProtectedPath } from '@/features/compat/types'
import { describeError } from '@/lib/i18n'

function Section({ title, hint, open = false, children }: { title: string; hint?: string; open?: boolean; children: React.ReactNode }) {
  return (
    <details open={open} className="group rounded-xl border border-border bg-card">
      <summary className="flex cursor-pointer flex-col gap-0.5 px-4 py-3 outline-none focus-visible:ring-2 focus-visible:ring-primary/40 focus-visible:ring-inset">
        <span className="text-sm font-medium">{title}</span>
        {hint !== undefined && <span className="text-xs text-muted-foreground">{hint}</span>}
      </summary>
      <div className="flex flex-col gap-3 border-t border-border px-4 py-3">{children}</div>
    </details>
  )
}

/// 声明式余额探针的一句话摘要：请求哪些地址、剩余额度从哪里读。
function ProbeSummary({ probe }: { probe: BalanceProbe | undefined }) {
  const { t } = useTranslation()
  if (probe === undefined) return <span className="text-muted-foreground">{t('compat:probeGeneric')}</span>
  const reads = [
    ['remaining', probe.remaining],
    ['total', probe.total],
    ['used', probe.used],
  ] as const
  return (
    <div className="flex flex-col gap-1">
      {probe.requests.map((request, index) => (
        <code key={index} className="break-all text-xs">
          GET {request.root === true ? `${t('compat:probeRoot')} ` : ''}
          {request.path}
        </code>
      ))}
      <span className="flex flex-wrap gap-1">
        {reads.map(([name, amount]) =>
          amount === undefined ? null : (
            <span key={name} className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] leading-4 break-all">
              {t(`compat:probe_${name}`)} ← {amount.at}
              {amount.divide !== undefined && amount.divide !== 1 ? ` ÷ ${amount.divide}` : ''}
            </span>
          ),
        )}
      </span>
      {probe.doc?.verified_at !== undefined && (
        <span className="text-[11px] text-muted-foreground">{t('compat:probeVerified', { date: probe.doc.verified_at })}</span>
      )}
    </div>
  )
}

function PathList({ paths }: { paths: ProtectedPath[] }) {
  const { t } = useTranslation()
  const variant = { free: 'muted', sensitive: 'warning', billing_critical: 'destructive', protected: 'destructive' } as const
  return (
    <ul className="flex flex-wrap gap-x-4 gap-y-1.5 text-xs">
      {paths.map((entry) => (
        <li key={entry.path} className="flex items-center gap-1.5">
          <code className="break-all">{entry.path}</code>
          <Badge variant={variant[entry.class]}>{t(`compat:class_${entry.class}`)}</Badge>
          {entry.node_only && <Badge variant="outline">{t('compat:nodeOnly')}</Badge>}
        </li>
      ))}
    </ul>
  )
}

/// 参考：引擎能做什么（操作 / 选择器 / 受保护字段 / 限额）、内置厂商的端点与探针、现成的配方。
/// 操作与字段读后端的自描述目录，Rust 里新增一个操作这里自动多一行。
export function ReferenceTab() {
  const { t } = useTranslation()
  const catalog = useCatalog()
  const vendors = useCompatVendors(true)

  if (catalog.isError) return <ErrorState message={describeError(catalog.error)} onRetry={() => void catalog.refetch()} />
  if (catalog.isPending) return <Skeleton className="h-64 w-full" />
  const doc = catalog.data
  const vendorRows = vendors.data?.data ?? []
  return (
    <div className="flex flex-col gap-3">
      <Section title={t('compat:refVendors')} hint={t('compat:refVendorsHint')} open>
        {vendors.isError ? (
          <ErrorState message={describeError(vendors.error)} onRetry={() => void vendors.refetch()} />
        ) : (
          <Table aria-label={t('compat:refVendors')}>
            <THead>
              <Tr>
                <Th>{t('compat:colVendor')}</Th>
                <Th>{t('compat:colHosts')}</Th>
                <Th>{t('compat:colEndpoints')}</Th>
                <Th>{t('compat:colProbe')}</Th>
                <Th numeric>{t('compat:colVendorRules')}</Th>
              </Tr>
            </THead>
            <TBody>
              {vendorRows.map((vendor) => (
                <Tr key={vendor.id}>
                  <Td>
                    <div className="flex flex-col">
                      <span className="font-medium">{vendor.display_name}</span>
                      <code className="text-xs text-muted-foreground">{vendor.id}</code>
                    </div>
                  </Td>
                  <Td className="max-w-48 text-xs break-all text-muted-foreground">{vendor.hosts.join(', ')}</Td>
                  <Td>
                    <ul className="flex flex-col gap-0.5 text-xs">
                      {vendor.endpoints.map((endpoint) => (
                        <li key={`${endpoint.protocol}:${endpoint.base}`} className="flex flex-wrap items-center gap-1.5">
                          <Badge variant="outline">{endpoint.protocol}</Badge>
                          <code className="break-all">{endpoint.base}</code>
                          {endpoint.label !== undefined && <span className="text-muted-foreground">{endpoint.label}</span>}
                        </li>
                      ))}
                    </ul>
                  </Td>
                  <Td className="max-w-72">
                    <ProbeSummary probe={vendor.probes.balance} />
                  </Td>
                  <Td numeric>{vendor.rules}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
        <p className="text-xs text-muted-foreground">{t('compat:refVendorsNote')}</p>
      </Section>

      <Section title={t('compat:refOps')} hint={t('compat:refOpsHint', { n: doc.ops.length })}>
        <Table aria-label={t('compat:refOps')}>
          <THead>
            <Tr>
              <Th>{t('compat:colOp')}</Th>
              <Th>{t('compat:colPhases')}</Th>
              <Th>{t('compat:colDescription')}</Th>
              <Th>{t('compat:colFields')}</Th>
            </Tr>
          </THead>
          <TBody>
            {doc.ops.map((op) => (
              <Tr key={op.name}>
                <Td className="align-top">
                  <div className="flex flex-col">
                    <span className="font-medium">{t(`compat:op_${op.name}`, { defaultValue: op.name })}</span>
                    <code className="text-xs text-muted-foreground">{op.name}</code>
                  </div>
                </Td>
                <Td className="align-top">
                  <div className="flex flex-wrap gap-1">
                    {op.phases.map((phase) => (
                      <Badge key={phase} variant="muted">{t(`compat:phase_${phase}`, { defaultValue: phase })}</Badge>
                    ))}
                  </div>
                </Td>
                <Td className="max-w-72 align-top text-xs text-muted-foreground">
                  {t(`compat:opDesc_${op.name}`, { defaultValue: op.summary })}
                </Td>
                <Td className="align-top">
                  <ul className="flex flex-col gap-0.5 text-xs">
                    {op.fields.map((field) => (
                      <li key={field.name} className="flex flex-wrap items-baseline gap-1.5">
                        <code>{field.name}{field.required ? '*' : ''}</code>
                        <span className="text-muted-foreground">{field.type}</span>
                      </li>
                    ))}
                  </ul>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      </Section>

      <Section title={t('compat:refPhases')} hint={t('compat:refPhasesHint')}>
        <ul className="flex flex-col gap-2 text-xs">
          {doc.phases.map((phase) => (
            <li key={phase.id} className="flex flex-wrap items-center gap-2">
              <code className="w-24 shrink-0">{phase.id}</code>
              <Badge variant={phase.enabled ? 'success' : 'muted'}>{phase.enabled ? t('compat:enabled') : t('compat:reserved')}</Badge>
              <span className="min-w-0 text-muted-foreground">{t(`compat:phaseHint_${phase.id}`, { defaultValue: phase.summary })}</span>
            </li>
          ))}
        </ul>
      </Section>

      <Section title={t('compat:refSelectors')} hint={t('compat:refSelectorsHint')}>
        <ul className="grid gap-x-6 gap-y-1.5 text-xs sm:grid-cols-2">
          {doc.selector.fields.map((field) => (
            <li key={field.name} className="flex flex-wrap items-baseline gap-1.5">
              <code>{field.name}</code>
              <span className="text-muted-foreground">{field.type}</span>
              {field.values !== undefined && <span className="break-all text-muted-foreground">{field.values.join(' · ')}</span>}
            </li>
          ))}
        </ul>
        <p className="text-xs font-medium">{t('compat:refWhen')}</p>
        <ul className="flex flex-col gap-1 text-xs">
          {doc.when.fields.map((field) => (
            <li key={field.name} className="flex flex-wrap items-baseline gap-1.5">
              <code>{field.name}</code>
              <span className="text-muted-foreground">{field.type}</span>
            </li>
          ))}
        </ul>
        <p className="text-xs font-medium">{t('compat:refSlots')}</p>
        <p className="flex flex-wrap gap-1.5 text-xs">
          {doc.usage_slots.map((slot) => (
            <code key={slot.name} title={slot.pointer} className="rounded bg-muted px-1.5 py-0.5">{slot.name}</code>
          ))}
        </p>
        <p className="text-xs font-medium">{t('compat:refFacts')}</p>
        <p className="flex flex-wrap gap-1.5 text-xs">
          {doc.capability_facts.map((fact) => (
            <code key={fact} className="rounded bg-muted px-1.5 py-0.5">{fact}</code>
          ))}
        </p>
      </Section>

      <Section title={t('compat:refProtected')} hint={t('compat:refProtectedHint')}>
        <p className="text-xs font-medium">{t('compat:phase_request')}</p>
        <PathList paths={doc.protected_paths.request} />
        <p className="text-xs font-medium">{t('compat:phase_response')}</p>
        <PathList paths={doc.protected_paths.response} />
      </Section>

      <Section title={t('compat:refRecipes')} hint={t('compat:refRecipesHint', { n: doc.recipes.length })}>
        <ul className="grid gap-3 lg:grid-cols-2">
          {doc.recipes.map((recipe) => (
            <li key={recipe.id} className="flex min-w-0 flex-col gap-1.5 rounded-lg border border-border p-3">
              <div className="flex items-center justify-between gap-2">
                <span className="text-sm font-medium">{recipeTitle(t, recipe)}</span>
                <CopyButton value={pretty(recipe.rule)} label={t('compat:copyRecipe')} />
              </div>
              <pre className="max-h-48 overflow-auto rounded-md bg-muted/40 p-2 font-mono text-xs leading-5">{pretty(recipe.rule)}</pre>
            </li>
          ))}
        </ul>
      </Section>

      <Section title={t('compat:refLimits')}>
        <dl className="grid gap-x-6 gap-y-1 text-xs sm:grid-cols-2">
          {Object.entries(doc.limits).map(([name, value]) => (
            <div key={name} className="flex items-baseline justify-between gap-3">
              <dt className="text-muted-foreground">{t(`compat:limit_${name}`, { defaultValue: name })}</dt>
              <dd className="tabular-nums">{value}</dd>
            </div>
          ))}
        </dl>
      </Section>
    </div>
  )
}
