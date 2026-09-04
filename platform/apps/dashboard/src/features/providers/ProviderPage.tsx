import type { ProviderRequest } from './provider-types'
import {
  Analytics03Icon,
  ArrowLeft01Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useState } from 'react'
import { Link, NavLink, useParams } from 'react-router-dom'
import { ProviderIcon } from './provider-icons'
import { PROVIDER_LABELS } from './provider-metadata'
import {
  useProviderDetail,
  useProviderRequests,
  useProviderUsage,
} from './provider-queries'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
  timeStyle: 'medium',
})

export function ProviderPage() {
  const { providerId = '' } = useParams()
  const [usageDisplay, setUsageDisplay] = useState<'cost' | 'tokens'>('cost')
  const providerPath = `/providers/${encodeURIComponent(providerId)}`
  const detail = useProviderDetail(providerId)
  const account = detail.data?.provider
  const isAccountsPending = detail.isPending
  const accountName =
    account?.name ?? (isAccountsPending ? 'Loading…' : providerId)
  const usage = useProviderUsage(providerId)
  const requests = useProviderRequests(providerId)
  const requestItems = [...new Map((requests.data?.pages.flatMap((page) => page.items) ?? []).map((item) => [item.id, item])).values()]
  const totalTokens =
    usage.data === undefined
      ? 0
      : usage.data.tokens.input +
        usage.data.tokens.output +
        usage.data.tokens.cache_read +
        usage.data.tokens.cache_write
  const cacheHitPercent =
    usage.data === undefined
      ? 0
      : calculateCacheHitPercent(
          usage.data.tokens.input,
          usage.data.tokens.cache_read,
        )

  return (
    <div className="provider-detail-shell">
      <aside className="provider-detail-sidebar">
        <Link
          className="provider-detail-nav-item provider-detail-back"
          to="/providers"
        >
          <span className="nav-icon-frame" aria-hidden="true">
            <HugeiconsIcon
              className="nav-item-icon"
              icon={ArrowLeft01Icon}
              size={16}
              color="currentColor"
              strokeWidth={1.5}
            />
          </span>
          <span className="nav-item-label">Back to Dashboard</span>
        </Link>

        <nav
          className="provider-detail-nav"
          aria-label="Provider account"
        >
          <NavLink
            end
            className={({ isActive }) =>
              `provider-detail-nav-item${
                isActive ? ' provider-detail-nav-item--active' : ''
              }`
            }
            to={providerPath}
          >
            <span className="nav-icon-frame" aria-hidden="true">
              <HugeiconsIcon
                className="nav-item-icon"
                icon={Analytics03Icon}
                size={16}
                color="currentColor"
                strokeWidth={1.5}
              />
            </span>
            <span className="nav-item-label">Overview</span>
          </NavLink>
        </nav>
      </aside>

      <main className="provider-detail-main">
        <div className="provider-detail-container provider-overview-page">
          <header className="provider-detail-header">
            <h1 className="cursor-page-title">{accountName} Overview</h1>
            <div className="provider-overview-meta">
              {account !== undefined ? (
                <div className="provider-overview-identity">
                  <ProviderIcon
                    provider={account.provider}
                    width={14}
                    height={14}
                  />
                  <span>{PROVIDER_LABELS[account.provider]}</span>
                </div>
              ) : (
                <span />
              )}
              <div
                className="provider-usage-toggle"
                role="group"
                aria-label="Usage value"
              >
                <button
                  type="button"
                  className="provider-usage-toggle-option"
                  aria-pressed={usageDisplay === 'cost'}
                  onClick={() => setUsageDisplay('cost')}
                >
                  Cost
                </button>
                <button
                  type="button"
                  className="provider-usage-toggle-option"
                  aria-pressed={usageDisplay === 'tokens'}
                  onClick={() => setUsageDisplay('tokens')}
                >
                  Tokens
                </button>
              </div>
            </div>
          </header>

          {detail.isError && <OverviewError label="Couldn’t load provider details" message={detail.error.message} onRetry={() => void detail.refetch()} />}
          <section className="providers-section" aria-label="Usage totals">
            {usage.isPending ? (
              <OverviewState>Loading usage…</OverviewState>
            ) : null}
            {usage.isError ? (
              <OverviewError
                label="Couldn’t load usage"
                message={usage.error.message}
                onRetry={() => void usage.refetch()}
              />
            ) : null}
            {usage.data !== undefined ? (
              <div className="provider-usage-cards">
                <div className="provider-usage-primary-cards">
                  <UsageCard
                    title="Total Requests"
                    value={formatNumber(usage.data.request_count)}
                  />
                  <UsageCard
                    title="Total"
                    value={formatUsageValue(
                      usageDisplay,
                      totalTokens,
                      usage.data.costs.total,
                    )}
                  />
                  <UsageCard
                    title="Cache Hit Percent"
                    value={formatPercent(cacheHitPercent)}
                  />
                </div>

                <div className="provider-usage-breakdown-cards">
                  <UsageCard
                    title="Inputs"
                    value={formatUsageValue(
                      usageDisplay,
                      usage.data.tokens.input,
                      usage.data.costs.input,
                    )}
                  />
                  <UsageCard
                    title="Outputs"
                    value={formatUsageValue(
                      usageDisplay,
                      usage.data.tokens.output,
                      usage.data.costs.output,
                    )}
                  />
                  <UsageCard
                    title="Cache Reads"
                    value={formatUsageValue(
                      usageDisplay,
                      usage.data.tokens.cache_read,
                      usage.data.costs.cache_read,
                    )}
                  />
                  <UsageCard
                    title="Cache Writes"
                    value={formatUsageValue(
                      usageDisplay,
                      usage.data.tokens.cache_write,
                      usage.data.costs.cache_write,
                    )}
                  />
                </div>
              </div>
            ) : null}
            <p className="provider-analytics-note">All-time usage recorded by this gateway. Costs reflect recorded model pricing, not your provider invoice.</p>
          </section>

          <section
            className="providers-section"
            aria-labelledby="provider-requests-title"
          >
            <div className="providers-section-header">
              <h2 id="provider-requests-title">Recent requests</h2>
            </div>

            <div className="providers-table-wrap" role="region" aria-label="Recent provider requests" tabIndex={0}>
              <table className="providers-table provider-requests-table">
                <thead>
                  <tr>
                    <th scope="col">ID</th>
                    <th scope="col">Completed</th>
                    <th scope="col">Model ID</th>
                    <th scope="col">Total</th>
                    <th scope="col">Input</th>
                    <th scope="col">Output</th>
                    <th scope="col">Cache Read</th>
                    <th scope="col">Cache Write</th>
                    <th scope="col">Cache Hit Percent</th>
                  </tr>
                </thead>
                <tbody aria-live="polite" aria-busy={requests.isFetching}>
                  {requests.isPending ? (
                    <RequestTableMessage>Loading requests…</RequestTableMessage>
                  ) : null}
                  {requests.isError ? (
                    <RequestTableError
                      message={requests.error.message}
                      onRetry={() => void requests.refetch()}
                    />
                  ) : null}
                  {!requests.isPending &&
                  !requests.isError &&
                  requestItems.length === 0 ? (
                    <RequestTableMessage>No requests yet</RequestTableMessage>
                  ) : null}
                  {requestItems.length > 0
                    ? requestItems.map((request) => (
                        <ProviderRequestRow
                          key={request.id}
                          request={request}
                          usageDisplay={usageDisplay}
                        />
                      ))
                    : null}
                </tbody>
              </table>
            </div>

            {requests.hasNextPage ? (
              <button
                type="button"
                className="cursor-button provider-requests-load-more"
                disabled={requests.isFetching}
                onClick={() => { if (!requests.isFetching) void requests.fetchNextPage() }}
              >
                {requests.isFetchingNextPage ? 'Loading…' : 'Load more'}
              </button>
            ) : null}
          </section>
        </div>
      </main>
    </div>
  )
}

function UsageCard({
  title,
  value,
}: {
  title: string
  value: string
}) {
  return (
    <article className="provider-usage-card">
      <h3>{title}</h3>
      <div className="provider-usage-card-value">{value}</div>
    </article>
  )
}

function ProviderRequestRow({
  request,
  usageDisplay,
}: {
  request: ProviderRequest
  usageDisplay: 'cost' | 'tokens'
}) {
  const usage = request.usage
  const totalTokens =
    usage === null || [usage.input, usage.output, usage.cache_read, usage.cache_write].every((value) => value == null)
      ? undefined
      : (usage.input ?? 0) +
        (usage.output ?? 0) +
        (usage.cache_read ?? 0) +
        (usage.cache_write ?? 0)
  const cacheHitPercent =
    usage === null || (usage.input == null && usage.cache_read == null)
      ? undefined
      : calculateCacheHitPercent(usage.input ?? 0, usage.cache_read ?? 0)

  return (
    <tr>
      <td title={request.status}>{request.id}</td>
      <td>
        {request.completed_at ? <time dateTime={request.completed_at}>{formatDate(request.completed_at)}</time> : request.status === 'running' ? 'Running…' : '—'}
      </td>
      <td>{request.response_model ?? request.requested_model}</td>
      <td>
        {formatRequestUsageValue(
          usageDisplay,
          totalTokens,
          usage?.cost?.total,
        )}
      </td>
      <td>
        {formatRequestUsageValue(
          usageDisplay,
          usage?.input,
          usage?.cost?.input,
        )}
      </td>
      <td>
        {formatRequestUsageValue(
          usageDisplay,
          usage?.output,
          usage?.cost?.output,
        )}
      </td>
      <td>
        {formatRequestUsageValue(
          usageDisplay,
          usage?.cache_read,
          usage?.cost?.cache_read,
        )}
      </td>
      <td>
        {formatRequestUsageValue(
          usageDisplay,
          usage?.cache_write,
          usage?.cost?.cache_write,
        )}
      </td>
      <td>
        {cacheHitPercent === undefined
          ? '—'
          : formatPercent(cacheHitPercent)}
      </td>
    </tr>
  )
}

function OverviewState({ children }: { children: string }) {
  return <div className="provider-overview-state">{children}</div>
}

function OverviewError({
  label,
  message,
  onRetry,
}: {
  label: string
  message: string
  onRetry: () => void
}) {
  return (
    <div className="provider-overview-state" role="alert" title={message}>
      <span>{label}</span>
      <button type="button" className="providers-retry-button" onClick={onRetry}>
        Retry
      </button>
    </div>
  )
}

function RequestTableMessage({ children }: { children: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={9}>
        {children}
      </td>
    </tr>
  )
}

function RequestTableError({
  message,
  onRetry,
}: {
  message: string
  onRetry: () => void
}) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={9} title={message}>
        <span>Couldn&apos;t load requests</span>
        <button
          type="button"
          className="providers-retry-button"
          onClick={onRetry}
        >
          Retry
        </button>
      </td>
    </tr>
  )
}

function formatDate(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : DATE_FORMATTER.format(date)
}

function formatNumber(value: number) {
  return value.toLocaleString()
}

function calculateCacheHitPercent(inputTokens: number, cacheReadTokens: number) {
  const eligibleInputTokens = inputTokens + cacheReadTokens
  return eligibleInputTokens === 0
    ? 0
    : (cacheReadTokens / eligibleInputTokens) * 100
}

function formatPercent(value: number) {
  return `${value.toLocaleString(undefined, { maximumFractionDigits: 2 })}%`
}

function formatUsageValue(
  display: 'cost' | 'tokens',
  tokens: number,
  cost: number,
) {
  return display === 'cost' ? formatCost(cost) : formatNumber(tokens)
}

function formatRequestUsageValue(
  display: 'cost' | 'tokens',
  tokens: number | null | undefined,
  cost: number | null | undefined,
) {
  const value = display === 'cost' ? cost : tokens
  if (value == null) {
    return '—'
  }
  return display === 'cost' ? formatCost(value) : formatNumber(value)
}

function formatCost(value: number) {
  return `$${value.toFixed(5)}`
}
