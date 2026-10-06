import { useTranslation } from 'react-i18next'
import { formatCount } from '@/lib/money'

export function CacheTokenValue({ value }: { value: { tokens: number | null; partial: boolean } }) {
  const { t, i18n } = useTranslation()
  return <>{value.tokens == null ? '-' : formatCount(value.tokens, i18n.language)}{value.partial && <span className="ml-1 text-[10px] font-normal text-warning">{t('portal:cachePartial')}</span>}</>
}
