import {
  ArrowLeft01Icon,
  MoreHorizontalIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useMemo } from 'react'
import { useNavigate } from 'react-router-dom'
import { useMachineInventory } from './machine-queries'

type SandboxListItem = {
  id: string
  name: string
  createdFrom: string
  createdAt: string
}

type SnapshotListItem = {
  id: string
  name: string
  sandboxId: string
  createdAt: string
}

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})

const SANDBOXES: SandboxListItem[] = []
const SNAPSHOTS: SnapshotListItem[] = []

export function SandboxAccountPage({ accountId }: { accountId: string }) {
  const navigate = useNavigate()
  const { data, isPending } = useMachineInventory()
  const account = useMemo(
    () => data?.connector_accounts.find((item) => item.id === accountId),
    [accountId, data?.connector_accounts],
  )
  const heading = account?.name ?? (isPending ? 'Loading…' : 'Sandbox account')

  return (
    <section className="sandbox-account-page" aria-labelledby="sandbox-account-heading">
      <header className="sandbox-account-page-header">
        <button
          type="button"
          className="cursor-button cursor-button--ghost cursor-icon-button sandbox-account-back-button"
          aria-label="Back to machines"
          onClick={() => navigate('/machines')}
        >
          <HugeiconsIcon
            icon={ArrowLeft01Icon}
            size={17}
            color="currentColor"
            strokeWidth={1.5}
            aria-hidden="true"
          />
        </button>
        <h1 id="sandbox-account-heading" className="cursor-page-title">
          {heading}
        </h1>
      </header>

      <div className="sandbox-account-tables">
        <AccountTableSection title="Sandboxes" showAdd>
          <table className="providers-table sandbox-resource-table">
            <colgroup>
              <col className="sandbox-id-column" />
              <col className="sandbox-name-column" />
              <col className="sandbox-source-column" />
              <col className="sandbox-created-column" />
              <col className="sandbox-actions-column" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Id</th>
                <th scope="col">Name</th>
                <th scope="col">Created from</th>
                <th scope="col">Created at</th>
                <th scope="col">
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {SANDBOXES.length === 0 ? (
                <EmptyResourceRow colSpan={5}>No sandboxes</EmptyResourceRow>
              ) : (
                SANDBOXES.map((sandbox) => (
                  <SandboxResourceRow key={sandbox.id} sandbox={sandbox} />
                ))
              )}
            </tbody>
          </table>
        </AccountTableSection>

        <AccountTableSection title="Snapshots">
          <table className="providers-table sandbox-resource-table">
            <colgroup>
              <col className="snapshot-id-column" />
              <col className="snapshot-name-column" />
              <col className="snapshot-sandbox-column" />
              <col className="snapshot-created-column" />
              <col className="sandbox-actions-column" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Id</th>
                <th scope="col">Name</th>
                <th scope="col">Sandbox id</th>
                <th scope="col">Created at</th>
                <th scope="col">
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {SNAPSHOTS.length === 0 ? (
                <EmptyResourceRow colSpan={5}>No snapshots</EmptyResourceRow>
              ) : (
                SNAPSHOTS.map((snapshot) => (
                  <SnapshotResourceRow key={snapshot.id} snapshot={snapshot} />
                ))
              )}
            </tbody>
          </table>
        </AccountTableSection>
      </div>
    </section>
  )
}

function AccountTableSection({
  children,
  showAdd = false,
  title,
}: {
  children: React.ReactNode
  showAdd?: boolean
  title: string
}) {
  return (
    <section className="sandbox-account-table-section" aria-label={title}>
      <header className="providers-section-header">
        <h2>{title}</h2>
        {showAdd ? (
          <button type="button" className="cursor-button provider-add-button">
            Add
          </button>
        ) : null}
      </header>
      <div className="providers-table-wrap">{children}</div>
    </section>
  )
}

function EmptyResourceRow({
  children,
  colSpan,
}: {
  children: string
  colSpan: number
}) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={colSpan}>
        {children}
      </td>
    </tr>
  )
}

function SandboxResourceRow({ sandbox }: { sandbox: SandboxListItem }) {
  return (
    <tr>
      <td><span className="provider-detail">{sandbox.id}</span></td>
      <td><span className="provider-name">{sandbox.name}</span></td>
      <td><span className="provider-detail">{sandbox.createdFrom}</span></td>
      <td>
        <time className="provider-detail" dateTime={sandbox.createdAt}>
          {formatDate(sandbox.createdAt)}
        </time>
      </td>
      <td className="provider-actions-cell">
        <ResourceActionsButton name={sandbox.name} />
      </td>
    </tr>
  )
}

function SnapshotResourceRow({ snapshot }: { snapshot: SnapshotListItem }) {
  return (
    <tr>
      <td><span className="provider-detail">{snapshot.id}</span></td>
      <td><span className="provider-name">{snapshot.name}</span></td>
      <td><span className="provider-detail">{snapshot.sandboxId}</span></td>
      <td>
        <time className="provider-detail" dateTime={snapshot.createdAt}>
          {formatDate(snapshot.createdAt)}
        </time>
      </td>
      <td className="provider-actions-cell">
        <ResourceActionsButton name={snapshot.name} />
      </td>
    </tr>
  )
}

function ResourceActionsButton({ name }: { name: string }) {
  return (
    <button
      type="button"
      className="cursor-button cursor-button--ghost cursor-icon-button provider-actions-trigger"
      aria-label={`Actions for ${name}`}
      aria-haspopup="menu"
    >
      <HugeiconsIcon
        icon={MoreHorizontalIcon}
        size={16}
        color="currentColor"
        strokeWidth={1.5}
        aria-hidden="true"
      />
    </button>
  )
}

function formatDate(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : DATE_FORMATTER.format(date)
}
