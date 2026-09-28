import { Check, Copy } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { IconButton } from '@/components/ui/icon-button'
import { toast } from '@/components/ui/toast'
import { ApiError, apiFetch } from '@/lib/api'

export type KeyCopyStatus = 'available' | 'not_saved' | 'unavailable'

export function KeyCopyButton({ id, name, status, hasSession }: {
  id: number; name: string; status?: KeyCopyStatus; hasSession: boolean
}) {
  const { t } = useTranslation()
  const [busy, setBusy] = useState(false)
  const [copied, setCopied] = useState(false)
  const inFlight = useRef(false)
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])

  const copy = async () => {
    if (inFlight.current) return
    if (status === 'not_saved') { toast.error(t('portal:keyCopyNotSaved')); return }
    if (status !== 'available') { toast.error(t('portal:keyCopyUnavailable')); return }
    if (!hasSession) { toast.error(t('portal:keyCopySessionRequired')); return }
    if (!navigator.clipboard) { toast.error(t('portal:keyCopyFailed')); return }
    inFlight.current = true
    setBusy(true)
    setCopied(false)
    window.clearTimeout(timer.current)
    try {
      // Plaintext stays in this click's promise only: no query cache, React state or storage.
      const secret = apiFetch<{ api_key: string }>(`/auth/keys/${id}/copy`, { method: 'POST', body: {} })
        .then((result) => {
          if (typeof result.api_key !== 'string' || !result.api_key.startsWith('sk-okapi-')) throw new Error('invalid_key_response')
          return result.api_key
        })
      // Safari requires clipboard.write to start inside the original user gesture.
      if (typeof ClipboardItem !== 'undefined' && navigator.clipboard.write) {
        const blob = secret.then((value) => new Blob([value], { type: 'text/plain' }))
        // Also handle a constructor/write exception before the browser consumes the promise.
        void blob.catch(() => undefined)
        const [fetched, written] = await Promise.allSettled([
          secret, navigator.clipboard.write([new ClipboardItem({ 'text/plain': blob })]),
        ])
        // Prefer a session/decryption error over the clipboard's generic rejected-blob error.
        if (fetched.status === 'rejected') throw fetched.reason
        if (written.status === 'rejected') throw written.reason
      } else {
        await navigator.clipboard.writeText(await secret)
      }
      setCopied(true)
      toast.success(t('common:copied'))
      timer.current = window.setTimeout(() => setCopied(false), 1500)
    } catch (error) {
      const message = error instanceof ApiError
        ? error.status === 401 || error.code === 'key_copy_session_mismatch'
          ? 'keyCopySessionRequired'
          : error.code === 'key_copy_not_saved' ? 'keyCopyNotSaved' : 'keyCopyUnavailable'
        : 'keyCopyFailed'
      toast.error(t(`portal:${message}`))
    } finally {
      inFlight.current = false
      setBusy(false)
    }
  }

  return <IconButton
    icon={copied ? Check : Copy}
    label={copied ? t('common:copied') : t('portal:keyCopy', { name, id })}
    loading={busy}
    className={copied ? 'text-success hover:text-success' : undefined}
    onClick={() => void copy()}
  />
}
