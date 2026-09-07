import { useMutation } from '@tanstack/react-query'
import { ClipboardPaste, RefreshCw } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Label } from '@/components/ui/input'
import { Tabs } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'
import { toast } from '@/components/ui/toast'
import { RatioSyncPanel } from '@/features/models/RatioSyncPanel'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

type ImportTab = 'paste' | 'sync'

/// 价格导入两条路：粘贴 new-api 导出的 JSON（全量覆盖），或在线拉取若干源逐项对比后择项应用
/// （IMPLEMENTATION §11.36）。粘贴页保留原始 JSON 输入是刻意的：粘贴的是**别处导出的产物**，
/// 用户不需要理解其结构，只要整段复制过来即可。
export function ImportDrawer({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { t } = useTranslation()
  const [tab, setTab] = useState<ImportTab>('paste')
  const [json, setJson] = useState('')

  const run = useMutation({
    mutationFn: () => {
      const parsed: unknown = JSON.parse(json)
      return apiFetch<{ imported: number; skipped: string[] }>('/admin/pricing/import-newapi', {
        method: 'POST',
        body: parsed,
      })
    },
    onSuccess: (r) => {
      toast.success(t('admin:importResult', { imported: r.imported, skipped: r.skipped.length }))
      onDone()
    },
    onError: (err) =>
      toast.error(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:importTitle')}
      description={tab === 'paste' ? t('admin:importDesc') : t('admin:syncTitle')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          {tab === 'paste' && (
            <Button disabled={json.trim() === '' || run.isPending} onClick={() => run.mutate()}>
              {t('admin:importRun')}
            </Button>
          )}
        </>
      }
    >
      <Tabs
        items={[
          { id: 'paste', label: t('admin:importTabPaste'), icon: ClipboardPaste },
          { id: 'sync', label: t('admin:importTabSync'), icon: RefreshCw },
        ]}
        active={tab}
        onChange={(id) => setTab(id as ImportTab)}
        className="mb-4"
      />
      {tab === 'paste' ? (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="import">{t('admin:importPaste')}</Label>
          <Textarea
            id="import"
            rows={16}
            className="font-mono text-xs"
            value={json}
            placeholder={t('admin:importPlaceholder')}
            onChange={(e) => setJson(e.target.value)}
          />
        </div>
      ) : (
        // 应用后只刷列表，不关抽屉：站长常常还要再拉一次核对
        <RatioSyncPanel onApplied={onDone} />
      )}
    </Drawer>
  )
}
