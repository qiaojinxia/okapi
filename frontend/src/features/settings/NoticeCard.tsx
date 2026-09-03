import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { NoticeBanner } from '@/components/notice-banner'
import { Button } from '@/components/ui/button'
import { toast } from '@/components/ui/toast'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

interface NoticeDraft {
  enabled: boolean
  title: string
  body: string
  level: 'info' | 'warning' | 'critical'
}

const EMPTY: NoticeDraft = { enabled: false, title: '', body: '', level: 'info' }

/// 站点公告编辑（settings.site_notice；new-api 系统公告 / 老 ok-api 公告系统的吸收，
/// 不新增表）。结构化表单而非 JSON 文本框：级别是三档枚举、正文要换行，
/// 让人手写 JSON 既发现不了可选项也校验不了拼写。
/// "保存"即发布：写入时盖 updated_at，前端横幅以它为已读锚点——重新发布会再次弹出。
export function NoticeCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<NoticeDraft | null>(null)

  const current = useQuery({
    queryKey: ['setting', 'site_notice'],
    queryFn: () => apiFetch<{ value: Partial<NoticeDraft> | null }>('/admin/settings/site_notice'),
  })
  const loaded: NoticeDraft = { ...EMPTY, ...current.data?.value }
  const form = draft ?? loaded

  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'site_notice', value: { ...form, updated_at: new Date().toISOString() } },
      }),
    onSuccess: () => {
      toast.success(t('admin:noticeSaved'))
      setDraft(null)
      void current.refetch()
      // 横幅读的是公开端点（60s 服务端缓存），本地立即失效以便预览刷新
      void queryClient.invalidateQueries({ queryKey: qk.notice })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const patch = (next: Partial<NoticeDraft>) => setDraft({ ...form, ...next })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:noticeTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <p className="text-xs text-muted-foreground">{t('admin:noticeHint')}</p>

        <Switch
          checked={form.enabled}
          onChange={(v) => patch({ enabled: v })}
          label={t('admin:noticeEnabled')}
          description={t('admin:noticeEnabledHint')}
        />
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <div className="flex flex-col gap-1.5 sm:col-span-2">
            <Label htmlFor="notice-title">{t('admin:noticeField_title')}</Label>
            <Input
              id="notice-title"
              value={form.title}
              maxLength={80}
              onChange={(e) => patch({ title: e.target.value })}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="notice-level">{t('admin:noticeField_level')}</Label>
            <Select
              id="notice-level"
              value={form.level}
              onChange={(v) => patch({ level: v as NoticeDraft['level'] })}
              options={[
                { value: 'info', label: t('admin:noticeLevel_info') },
                { value: 'warning', label: t('admin:noticeLevel_warning') },
                { value: 'critical', label: t('admin:noticeLevel_critical') },
              ]}
            />
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="notice-body">{t('admin:noticeField_body')}</Label>
          <Textarea
            id="notice-body"
            value={form.body}
            maxLength={4000}
            onChange={(e) => patch({ body: e.target.value })}
          />
          <span className="text-xs text-muted-foreground">{t('admin:noticeBodyHint')}</span>
        </div>

        <div className="flex items-center gap-3">
          <Button disabled={save.isPending || draft === null} onClick={() => save.mutate()}>
            {t('admin:noticePublish')}
          </Button>
        </div>

        {/* 当前线上效果：所见即所得，省一次切到门户去看 */}
        <div className="flex flex-col gap-1.5">
          <Label>{t('admin:noticePreview')}</Label>
          <NoticeBanner />
        </div>
      </CardContent>
    </Card>
  )
}
