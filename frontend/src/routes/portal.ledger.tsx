import { createFileRoute } from '@tanstack/react-router'
import { LedgerPage } from '@/features/ledger/LedgerPage'

export const Route = createFileRoute('/portal/ledger')({
  staticData: { fitViewport: true },
  component: LedgerPage,
})
