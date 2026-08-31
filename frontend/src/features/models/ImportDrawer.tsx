import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Label } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// new-api 价格导入。这里保留原始 JSON 输入是刻意的：粘贴的是**别处导出的产物**，
/// 用户不需要理解其结构，只要整段复制过来即可。
export function ImportDrawer({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { t } = useTranslation()
  const [json, setJson] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const run = useMutation({
    mutationFn: () => {
      const parsed: unknown = JSON.parse(json)
      return apiFetch<{ imported: number; skipped: string[] }>('/admin/pricing/import-newapi', {
        method: 'POST',
        body: parsed,
      })
    },
    onSuccess: (r) => {
      setMsg(t('admin:importResult', { imported: r.imported, skipped: r.skipped.length }))
      onDone()
    },
    onError: (err) =>
      setMsg(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:importTitle')}
      description={t('admin:importDesc')}
      footer={
        <>
          {msg !== null && <span className="mr-auto text-xs text-muted-foreground">{msg}</span>}
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button disabled={json.trim() === '' || run.isPending} onClick={() => run.mutate()}>
            {t('admin:importRun')}
          </Button>
        </>
      }
    >
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
    </Drawer>
  )
}
