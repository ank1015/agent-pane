import { AtIcon, Delete03Icon } from '@hugeicons/core-free-icons'
import { useMemo, useState } from 'react'
import { TableActionsMenu } from '../../components/TableActionsMenu'
import { AddSandboxEnvironmentTemplateDialog } from './AddSandboxEnvironmentTemplateDialog'
import {
  type SandboxEnvironmentTemplate,
  useSandboxEnvironmentTemplates,
  useSandboxSnapshots,
} from './machine-queries'
import { UpdateEnvironmentTemplateNameDialog } from './UpdateEnvironmentTemplateNameDialog'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})

type SandboxAccountEnvironmentTemplatesProps = {
  accountId: string
  accountName: string
  isAccountPending: boolean
}

export function SandboxAccountEnvironmentTemplates({
  accountId,
  accountName,
  isAccountPending,
}: SandboxAccountEnvironmentTemplatesProps) {
  const [isAddOpen, setIsAddOpen] = useState(false)
  const [renameTemplate, setRenameTemplate] =
    useState<SandboxEnvironmentTemplate | null>(null)
  const templates = useSandboxEnvironmentTemplates(accountId)
  const snapshots = useSandboxSnapshots(accountId)
  const snapshotIds = useMemo(
    () =>
      new Map(
        (snapshots.data ?? []).map((snapshot) => [
          snapshot.id,
          snapshot.provider_snapshot_id,
        ]),
      ),
    [snapshots.data],
  )

  return (
    <div className="cursor-container">
      <header className="page-header page-header--section">
        <h1 className="cursor-page-title">{accountName} Templates</h1>
      </header>

      <section
        className="providers-section"
        aria-labelledby="configured-environment-templates-title"
      >
        <div className="providers-section-header">
          <h2 id="configured-environment-templates-title">
            Configured Sandbox environment templates
          </h2>
          <button
            type="button"
            className="cursor-button provider-add-button"
            disabled={isAccountPending}
            onClick={() => setIsAddOpen(true)}
          >
            Add
          </button>
        </div>

        <div className="providers-table-wrap">
          <table className="providers-table environment-templates-table">
            <colgroup>
              <col className="environment-template-name-column" />
              <col className="environment-template-snapshot-column" />
              <col className="environment-template-path-column" />
              <col className="environment-template-created-column" />
              <col className="provider-actions-column" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Snapshot ID</th>
                <th scope="col">Path</th>
                <th scope="col">Created at</th>
                <th scope="col">
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              <TemplateRows
                isAccountPending={isAccountPending}
                query={templates}
                snapshotIds={snapshotIds}
                onUpdateName={setRenameTemplate}
              />
            </tbody>
          </table>
        </div>
      </section>

      <AddSandboxEnvironmentTemplateDialog
        accountId={accountId}
        open={isAddOpen}
        snapshots={snapshots.data ?? []}
        snapshotsError={snapshots.isError}
        snapshotsPending={snapshots.isPending}
        onClose={() => setIsAddOpen(false)}
      />
      {renameTemplate !== null ? (
        <UpdateEnvironmentTemplateNameDialog
          template={renameTemplate}
          onClose={() => setRenameTemplate(null)}
        />
      ) : null}
    </div>
  )
}

function TemplateRows({
  isAccountPending,
  query,
  snapshotIds,
  onUpdateName,
}: {
  isAccountPending: boolean
  query: ReturnType<typeof useSandboxEnvironmentTemplates>
  snapshotIds: Map<string, string>
  onUpdateName: (template: SandboxEnvironmentTemplate) => void
}) {
  if (isAccountPending || query.isPending) {
    return <TemplateTableMessage message="Loading environment templates…" />
  }
  if (query.isError) {
    return (
      <tr>
        <td className="providers-table-message" colSpan={5}>
          Couldn&apos;t load environment templates
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
    return <TemplateTableMessage message="No environment templates present" />
  }

  return query.data.map((template) => (
    <TemplateRow
      key={template.id}
      template={template}
      snapshotId={snapshotIds.get(template.snapshot_id) ?? template.snapshot_id}
      onUpdateName={onUpdateName}
    />
  ))
}

function TemplateRow({
  template,
  snapshotId,
  onUpdateName,
}: {
  template: SandboxEnvironmentTemplate
  snapshotId: string
  onUpdateName: (template: SandboxEnvironmentTemplate) => void
}) {
  const actions = [
    {
      icon: AtIcon,
      label: 'Update name',
      onSelect: () => onUpdateName(template),
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
        <span className="provider-name" title={template.name}>
          {template.name}
        </span>
      </td>
      <td>
        <span className="provider-detail" title={snapshotId}>
          {snapshotId}
        </span>
      </td>
      <td>
        <span className="provider-detail" title={template.cwd}>
          {template.cwd}
        </span>
      </td>
      <td>{DATE_FORMATTER.format(new Date(template.created_at))}</td>
      <td className="provider-actions-cell">
        <TableActionsMenu
          actions={actions}
          label={`Actions for ${template.name}`}
        />
      </td>
    </tr>
  )
}

function TemplateTableMessage({ message }: { message: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={5}>
        {message}
      </td>
    </tr>
  )
}
