import {
  AtIcon,
  Camera01Icon,
  Delete03Icon,
} from '@hugeicons/core-free-icons'
import { useState } from 'react'
import { TableActionsMenu } from '../../components/TableActionsMenu'
import { AddE2bSandboxDialog } from './AddE2bSandboxDialog'
import { CreateSnapshotDialog } from './CreateSnapshotDialog'
import type {
  SandboxConnectorAccount,
  SandboxMachine,
} from './machine-queries'
import { useSandboxMachines } from './machine-queries'
import { UpdateSandboxNameDialog } from './UpdateSandboxNameDialog'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})

type SandboxAccountSandboxesProps = {
  account?: SandboxConnectorAccount
  accountId: string
  accountName: string
  isAccountPending: boolean
}

export function SandboxAccountSandboxes({
  account,
  accountId,
  accountName,
  isAccountPending,
}: SandboxAccountSandboxesProps) {
  const [isAddOpen, setIsAddOpen] = useState(false)
  const [snapshotSandbox, setSnapshotSandbox] =
    useState<SandboxMachine | null>(null)
  const [renameSandbox, setRenameSandbox] = useState<SandboxMachine | null>(
    null,
  )
  const isE2b = account?.provider === 'e2b'
  const sandboxes = useSandboxMachines(accountId, isE2b)

  return (
    <div className="cursor-container">
      <header className="page-header page-header--section">
        <h1 className="cursor-page-title">{accountName} Sandboxes</h1>
      </header>

      <section
        className="providers-section"
        aria-labelledby="created-sandboxes-title"
      >
        <div className="providers-section-header">
          <h2 id="created-sandboxes-title">Created Sandboxes</h2>
          {isE2b ? (
            <button
              type="button"
              className="cursor-button provider-add-button"
              onClick={() => setIsAddOpen(true)}
            >
              Add
            </button>
          ) : null}
        </div>

        <div className="providers-table-wrap">
          <table className="providers-table sandboxes-table">
            <colgroup>
              <col className="sandbox-id-column" />
              <col className="sandbox-name-column" />
              <col className="sandbox-created-from-column" />
              <col className="sandbox-created-column" />
              <col className="provider-actions-column" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Sandbox ID</th>
                <th scope="col">Name</th>
                <th scope="col">Created from</th>
                <th scope="col">Created at</th>
                <th scope="col">
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              <SandboxRows
                isAccountPending={isAccountPending}
                isE2b={isE2b}
                query={sandboxes}
                onSnapshot={setSnapshotSandbox}
                onUpdateName={setRenameSandbox}
              />
            </tbody>
          </table>
        </div>
      </section>

      {isE2b ? (
        <AddE2bSandboxDialog
          accountId={accountId}
          open={isAddOpen}
          onClose={() => setIsAddOpen(false)}
        />
      ) : null}
      {snapshotSandbox !== null ? (
        <CreateSnapshotDialog
          accountId={accountId}
          open
          sandboxId={snapshotSandbox.sandbox_id}
          onClose={() => setSnapshotSandbox(null)}
        />
      ) : null}
      {renameSandbox !== null ? (
        <UpdateSandboxNameDialog
          sandbox={renameSandbox}
          onClose={() => setRenameSandbox(null)}
        />
      ) : null}
    </div>
  )
}

function SandboxRows({
  isAccountPending,
  isE2b,
  query,
  onSnapshot,
  onUpdateName,
}: {
  isAccountPending: boolean
  isE2b: boolean
  query: ReturnType<typeof useSandboxMachines>
  onSnapshot: (sandbox: SandboxMachine) => void
  onUpdateName: (sandbox: SandboxMachine) => void
}) {
  if (isAccountPending) {
    return <SandboxTableMessage message="Loading sandboxes…" />
  }
  if (!isE2b) {
    return <SandboxTableMessage message="No sandboxes present" />
  }
  if (query.isPending) {
    return <SandboxTableMessage message="Loading sandboxes…" />
  }
  if (query.isError) {
    return (
      <tr>
        <td className="providers-table-message" colSpan={5}>
          Couldn&apos;t load sandboxes
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
    return <SandboxTableMessage message="No sandboxes present" />
  }

  return query.data.map((sandbox) => (
    <SandboxRow
      key={sandbox.machine_id}
      sandbox={sandbox}
      onSnapshot={onSnapshot}
      onUpdateName={onUpdateName}
    />
  ))
}

function SandboxRow({
  sandbox,
  onSnapshot,
  onUpdateName,
}: {
  sandbox: SandboxMachine
  onSnapshot: (sandbox: SandboxMachine) => void
  onUpdateName: (sandbox: SandboxMachine) => void
}) {
  const actions = [
    {
      icon: Camera01Icon,
      label: 'Snapshot',
      onSelect: () => onSnapshot(sandbox),
    },
    {
      icon: AtIcon,
      label: 'Update name',
      onSelect: () => onUpdateName(sandbox),
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
        <span className="provider-detail" title={sandbox.sandbox_id}>
          {sandbox.sandbox_id}
        </span>
      </td>
      <td>
        <span className="provider-name" title={sandbox.name}>
          {sandbox.name}
        </span>
      </td>
      <td>
        <span
          className="provider-detail"
          title={sandbox.created_from ?? undefined}
        >
          {sandbox.created_from ?? '—'}
        </span>
      </td>
      <td>{formatDate(sandbox.created_at)}</td>
      <td className="provider-actions-cell">
        <TableActionsMenu
          actions={actions}
          label={`Actions for ${sandbox.name}`}
        />
      </td>
    </tr>
  )
}

function SandboxTableMessage({ message }: { message: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={5}>
        {message}
      </td>
    </tr>
  )
}

function formatDate(timestamp: number) {
  return DATE_FORMATTER.format(new Date(timestamp))
}
