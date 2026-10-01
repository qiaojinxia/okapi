import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import { Save, ShieldCheck } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { ApiError, apiFetch, clearKey } from '@/lib/api'
import { describeError, switchLanguage } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

export interface AccountProfile {
  username: string
  email: string | null
  language: 'auto' | 'zh-CN' | 'en'
  created_at: string
}

export function BasicInfo({ active }: { active: boolean }) {
  const { t } = useTranslation()
  const client = useQueryClient()
  const navigate = useNavigate()
  const query = useQuery({ queryKey: qk.myProfile, queryFn: () => apiFetch<AccountProfile>('/api/me/profile'), retry: false, enabled: active, staleTime: 60_000 })
  if (query.error instanceof ApiError && query.error.status === 401) return <Alert tone="warning" action={<Link to="/" onClick={(event) => {
    event.preventDefault()
    void apiFetch('/auth/logout', { method: 'POST', body: {} }).catch(() => undefined).finally(() => {
      clearKey()
      client.clear()
      void navigate({ to: '/' })
    })
  }} className="font-medium text-primary hover:underline">{t('auth:tabPassword')}</Link>}>{t('profile:sessionRequired')}</Alert>
  if (query.isError) return <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} />
  if (!query.data) return <LoadingState />
  return <ProfileForm profile={query.data} />
}

function ProfileForm({ profile }: { profile: AccountProfile }) {
  const { t, i18n } = useTranslation()
  const client = useQueryClient()
  const [username, setUsername] = useState(profile.username)
  const [language, setLanguage] = useState(profile.language)
  const [error, setError] = useState<string | null>(null)
  const trimmed = username.trim()
  const valid = trimmed.length > 0 && Array.from(trimmed).length <= 64 && !Array.from(trimmed).some((char) => { const code = char.codePointAt(0)!; return code < 32 || (code >= 127 && code <= 159) })
  const dirty = trimmed !== profile.username || language !== profile.language
  const save = useMutation({
    mutationFn: () => apiFetch<AccountProfile>('/api/me/profile', { method: 'PATCH', body: { username: trimmed, language } }),
    onSuccess: (saved) => {
      client.setQueryData(qk.myProfile, saved)
      setUsername(saved.username)
      setLanguage(saved.language)
      setError(null)
      switchLanguage(saved.language === 'auto' ? navigator.language.startsWith('zh') ? 'zh-CN' : 'en' : saved.language)
      toast.success(t('profile:saved'))
    },
    onError: (err) => setError(describeError(err)),
  })
  return <div className="grid min-w-0 gap-4 xl:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
    <Card className="min-w-0">
      <CardHeader><CardTitle>{t('profile:basicInfo')}</CardTitle><CardDescription>{t('profile:basicInfoHint')}</CardDescription></CardHeader>
      <CardContent>
        <form className="space-y-5" onSubmit={(event) => { event.preventDefault(); if (valid && dirty && !save.isPending) save.mutate() }}>
          <Field label={t('auth:username')} htmlFor="profile-username" hint={t('profile:usernameHint')} error={!valid ? t('profile:invalidUsername') : undefined}>
            <Input id="profile-username" autoComplete="nickname" value={username} disabled={save.isPending} aria-invalid={!valid} onChange={(event) => { setUsername(event.target.value); setError(null) }} />
          </Field>
          <Field label={t('profile:language')} htmlFor="profile-language" hint={t('profile:languageHint')}>
            <Select id="profile-language" className="w-full" value={language} disabled={save.isPending} onChange={(value) => { setLanguage(value as AccountProfile['language']); setError(null) }} options={[
              { value: 'auto', label: t('profile:languageAuto') }, { value: 'zh-CN', label: t('common:langZh') }, { value: 'en', label: t('common:langEn') },
            ]} />
          </Field>
          {error && <Alert tone="destructive">{error}</Alert>}
          <div className="flex flex-wrap items-center gap-2 border-t border-border pt-4">
            <Button type="submit" loading={save.isPending} disabled={!valid || !dirty}><Save className="h-4 w-4" />{t('common:save')}</Button>
            <Button type="button" variant="ghost" disabled={!dirty || save.isPending} onClick={() => { setUsername(profile.username); setLanguage(profile.language); setError(null) }}>{t('profile:reset')}</Button>
          </div>
        </form>
      </CardContent>
    </Card>
    <Card className="min-w-0 self-start">
      <CardHeader><CardTitle>{t('profile:account')}</CardTitle><CardDescription>{t('profile:accountHint')}</CardDescription></CardHeader>
      <CardContent className="space-y-5">
        <dl className="space-y-4 text-sm">
          <div><dt className="text-xs text-muted-foreground">{t('auth:email')}</dt><dd className="mt-1 break-all">{profile.email || '—'}</dd></div>
          <div><dt className="text-xs text-muted-foreground">{t('profile:joined')}</dt><dd className="mt-1">{new Date(profile.created_at).toLocaleDateString(i18n.language)}</dd></div>
        </dl>
        <Link to="/portal/security" className="inline-flex min-h-11 items-center gap-2 rounded-lg border border-border px-3 text-sm hover:bg-accent focus-visible:outline-2 focus-visible:outline-primary"><ShieldCheck className="h-4 w-4" />{t('security:nav')}</Link>
      </CardContent>
    </Card>
  </div>
}
