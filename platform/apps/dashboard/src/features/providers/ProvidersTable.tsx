import { MoreHorizontalIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import { AddProviderDialog } from './AddProviderDialog'
import { PROVIDER_LABELS } from './provider-metadata'
import { ProviderIcon } from './provider-icons'
import { providerDetailOptions, useProviderAccounts } from './provider-queries'
import type { ProviderAccount } from './provider-types'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' })
const STATUS_LABELS: Record<ProviderAccount['status'], string> = {
  enabled: 'Enabled',
  disabled: 'Disabled',
  reauth_required: 'Sign-in required',
}

export function ProvidersTable() {
  const [adding, setAdding] = useState(false)
  const addRef = useRef<HTMLButtonElement>(null)
  const closeDialog = useCallback(() => {
    setAdding(false)
    addRef.current?.focus()
  }, [])
  const { data, error, isError, isPending, isFetching, refetch } = useProviderAccounts()

  return (
    <section className="providers-section" aria-labelledby="configured-providers-title">
      <div className="providers-section-header">
        <h2 id="configured-providers-title">User Configured Providers</h2>
        <button
          type="button"
          className="cursor-button provider-add-button"
          ref={addRef}
          onClick={() => setAdding(true)}
        >
          Add
        </button>
      </div>
      <div className="providers-table-wrap" role="region" aria-label="Configured provider accounts" tabIndex={0}>
        <table className="providers-table">
          <colgroup>
            <col className="provider-icon-column" />
            <col className="provider-name-column" />
            <col className="provider-type-column" />
            <col className="provider-status-column" />
            <col className="provider-added-column" />
            <col className="provider-actions-column" />
          </colgroup>
          <thead>
            <tr>
              <th scope="col"><span className="visually-hidden">Provider icon</span></th>
              <th scope="col">Name</th>
              <th scope="col">Provider</th>
              <th scope="col">Status</th>
              <th scope="col">Added</th>
              <th scope="col"><span className="visually-hidden">Actions</span></th>
            </tr>
          </thead>
          <tbody aria-live="polite" aria-busy={isPending && isFetching}>
            {isPending && (
              <TableMessage>{isFetching ? 'Loading providers...' : 'Waiting for a connection...'}</TableMessage>
            )}
            {isError && (
              <tr>
                <td className="providers-table-message" colSpan={6} title={error.message}>
                  <span role="alert">{data ? "Couldn't refresh providers" : "Couldn't load providers"}</span>
                  <button type="button" className="providers-retry-button" disabled={isFetching} onClick={() => void refetch()}>
                    {isFetching ? 'Retrying...' : 'Retry'}
                  </button>
                </td>
              </tr>
            )}
            {!isPending && !isError && data?.length === 0 && <TableMessage>No providers added</TableMessage>}
            {/* Keep the last successful inventory visible during refreshes and errors. */}
            {data?.map((account) => <ProviderRow key={account.id} account={account} />)}
          </tbody>
        </table>
      </div>
      {adding && <AddProviderDialog onClose={closeDialog} />}
    </section>
  )
}

function ProviderRow({ account }: { account: ProviderAccount }) {
  const client = useQueryClient()
  const prefetchDetail = () => {
    void client.prefetchQuery(providerDetailOptions(account.id))
  }

  return (
    <tr>
      <td className="provider-icon-cell"><ProviderIcon provider={account.provider} width={16} height={16} /></td>
      <td>
        <span className="provider-name-line">
          <Link
            className="provider-name provider-name-link"
            to={`/providers/${encodeURIComponent(account.id)}`}
            title={account.name}
            onMouseEnter={prefetchDetail}
            onFocus={prefetchDetail}
          >
            {account.name}
          </Link>
          {account.is_default && <span className="provider-default-badge">Default</span>}
        </span>
      </td>
      <td><span className="provider-detail">{PROVIDER_LABELS[account.provider]}</span></td>
      <td>
        <span className={`provider-detail provider-status provider-status--${account.status}`} title={STATUS_LABELS[account.status]}>
          {STATUS_LABELS[account.status]}
        </span>
      </td>
      <td><time className="provider-detail" dateTime={account.created_at}>{formatCreatedAt(account.created_at)}</time></td>
      <td className="provider-actions-cell">
        <button
          type="button"
          className="cursor-button cursor-button--ghost cursor-icon-button provider-actions-trigger"
          aria-label={`Actions for ${account.name}`}
          disabled
          title="Provider account actions are not available yet"
        >
          <HugeiconsIcon icon={MoreHorizontalIcon} size={16} color="currentColor" strokeWidth={1.5} aria-hidden="true" />
        </button>
      </td>
    </tr>
  )
}

function TableMessage({ children }: { children: string }) {
  return <tr><td className="providers-table-message" colSpan={6}>{children}</td></tr>
}

function formatCreatedAt(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : DATE_FORMATTER.format(date)
}
