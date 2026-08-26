import { AtIcon, Delete03Icon } from '@hugeicons/core-free-icons'
import { useState } from 'react'
import { TableActionsMenu } from '../../components/TableActionsMenu'
import {
  type SandboxSnapshot,
  useSandboxSnapshots,
} from './machine-queries'
import { UpdateSnapshotNameDialog } from './UpdateSnapshotNameDialog'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})

type SandboxAccountSnapshotsProps = {
  accountId: string
  accountName: string
  isAccountPending: boolean
}

export function SandboxAccountSnapshots({
  accountId,
  accountName,
  isAccountPending,
}: SandboxAccountSnapshotsProps) {
  const snapshots = useSandboxSnapshots(accountId)
  const [renameSnapshot, setRenameSnapshot] =
    useState<SandboxSnapshot | null>(null)

  return (
    <div className="cursor-container">
      <header className="page-header page-header--section">
        <h1 className="cursor-page-title">{accountName} Snapshots</h1>
      </header>

      <section
        className="providers-section"
        aria-labelledby="created-snapshots-title"
      >
        <div className="providers-section-header">
          <h2 id="created-snapshots-title">Created Snapshots</h2>
        </div>

        <div className="providers-table-wrap">
          <table className="providers-table snapshots-table">
            <colgroup>
              <col className="snapshot-name-column" />
              <col className="snapshot-id-column" />
              <col className="snapshot-created-from-column" />
              <col className="snapshot-created-column" />
              <col className="provider-actions-column" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Snapshot name</th>
                <th scope="col">Snapshot ID</th>
                <th scope="col">Created from</th>
                <th scope="col">Created at</th>
                <th scope="col">
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              <SnapshotRows
                isAccountPending={isAccountPending}
                query={snapshots}
                onUpdateName={setRenameSnapshot}
              />
            </tbody>
          </table>
        </div>
      </section>
      {renameSnapshot !== null ? (
        <UpdateSnapshotNameDialog
          snapshot={renameSnapshot}
          onClose={() => setRenameSnapshot(null)}
        />
      ) : null}
    </div>
  )
}

function SnapshotRows({
  isAccountPending,
  query,
  onUpdateName,
}: {
  isAccountPending: boolean
  query: ReturnType<typeof useSandboxSnapshots>
  onUpdateName: (snapshot: SandboxSnapshot) => void
}) {
  if (isAccountPending || query.isPending) {
    return <SnapshotTableMessage message="Loading snapshots…" />
  }
  if (query.isError) {
    return (
      <tr>
        <td className="providers-table-message" colSpan={5}>
          Couldn&apos;t load snapshots
          <button
            type="button"
            className="providers-retry-button"
            onClick={() => void query.refetch()}
          >
            Retry
          </button>
        </td>
      </tr>
    )
  }
  if (query.data.length === 0) {
    return <SnapshotTableMessage message="No snapshots present" />
  }

  return query.data.map((snapshot) => (
    <SnapshotRow
      key={snapshot.id}
      snapshot={snapshot}
      onUpdateName={onUpdateName}
    />
  ))
}

function SnapshotRow({
  snapshot,
  onUpdateName,
}: {
  snapshot: SandboxSnapshot
  onUpdateName: (snapshot: SandboxSnapshot) => void
}) {
  const actions = [
    {
      icon: AtIcon,
      label: 'Update name',
      onSelect: () => onUpdateName(snapshot),
    },
    {
      danger: true,
      icon: Delete03Icon,
      label: 'Delete',
      separatorBefore: true,
    },
  ]

  return (
    <tr>
      <td>
        <span className="provider-name" title={snapshot.name}>
          {snapshot.name}
        </span>
      </td>
      <td>
        <span
          className="provider-detail"
          title={snapshot.provider_snapshot_id}
        >
          {snapshot.provider_snapshot_id}
        </span>
      </td>
      <td>
        <span className="provider-detail" title={snapshot.sandbox_id}>
          {snapshot.sandbox_id}
        </span>
      </td>
      <td>{DATE_FORMATTER.format(new Date(snapshot.created_at))}</td>
      <td className="provider-actions-cell">
        <TableActionsMenu
          actions={actions}
          label={`Actions for ${snapshot.name}`}
        />
      </td>
    </tr>
  )
}

function SnapshotTableMessage({ message }: { message: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={5}>
        {message}
      </td>
    </tr>
  )
}
