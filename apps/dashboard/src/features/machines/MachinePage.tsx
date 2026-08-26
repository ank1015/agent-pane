import {
  ArrowLeft01Icon,
  Folder03Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useMemo, useState } from 'react'
import { Link, NavLink } from 'react-router-dom'
import { AddEnvironmentDialog } from './AddEnvironmentDialog'
import { DeleteEnvironmentDialog } from './DeleteEnvironmentDialog'
import { EnvironmentActionsMenu } from './EnvironmentActionsMenu'
import {
  type MachineEnvironment,
  useMachineEnvironments,
  useMachineInventory,
} from './machine-queries'
import { UpdateEnvironmentNameDialog } from './UpdateEnvironmentNameDialog'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})

export function MachinePage({ machineId }: { machineId: string }) {
  const [isAddDialogOpen, setIsAddDialogOpen] = useState(false)
  const [environmentToDelete, setEnvironmentToDelete] =
    useState<MachineEnvironment | null>(null)
  const [environmentToRename, setEnvironmentToRename] =
    useState<MachineEnvironment | null>(null)
  const machinePath = `/machines/${encodeURIComponent(machineId)}`
  const { data: inventory, isPending: isInventoryPending } =
    useMachineInventory()
  const {
    data: environments,
    error: environmentsError,
    isError: isEnvironmentsError,
    isPending: isEnvironmentsPending,
    refetch: refetchEnvironments,
  } = useMachineEnvironments(machineId)
  const machine = useMemo(
    () =>
      inventory?.machine_daemons.find(
        (candidate) => candidate.machine_id === machineId,
      ),
    [inventory?.machine_daemons, machineId],
  )
  const machineName =
    machine?.name ?? (isInventoryPending ? 'Loading…' : machineId)
  const heading = `${machineName} Environments`
  const workspaceRoot = machine?.descriptor.workspace_roots[0]
  const workspaceRootPath = formatWorkspaceRootPath(
    workspaceRoot?.uri,
    machine?.descriptor.path_convention,
  )

  return (
    <div className="cursor-shell machine-detail-shell">
      <aside className="cursor-sidebar dashboard-sidebar machine-detail-sidebar">
        <Link
          className="cursor-nav-item dashboard-nav-item machine-detail-back"
          to="/machines"
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
          className="cursor-nav dashboard-nav machine-detail-nav"
          aria-label="Machine"
        >
          <NavLink
            end
            className={({ isActive }) =>
              `cursor-nav-item dashboard-nav-item machine-detail-nav-item${
                isActive ? ' dashboard-nav-item--active' : ''
              }`
            }
            to={machinePath}
          >
            <span className="nav-icon-frame" aria-hidden="true">
              <HugeiconsIcon
                className="nav-item-icon"
                icon={Folder03Icon}
                size={16}
                color="currentColor"
                strokeWidth={1.5}
              />
            </span>
            <span className="nav-item-label">Environments</span>
          </NavLink>
        </nav>
      </aside>

      <main className="cursor-main machine-detail-main">
        <div className="cursor-container">
          <header className="page-header page-header--section">
            <h1 className="cursor-page-title">{heading}</h1>
          </header>

          <section
            className="providers-section"
            aria-labelledby="configured-environments-title"
          >
            <div className="providers-section-header">
              <h2 id="configured-environments-title">
                Configured Environments
              </h2>
              <button
                type="button"
                className="cursor-button provider-add-button"
                disabled={machine?.online !== true || workspaceRoot === undefined}
                onClick={() => setIsAddDialogOpen(true)}
              >
                Add
              </button>
            </div>

            <div className="providers-table-wrap">
              <table className="providers-table machine-environments-table">
                <colgroup>
                  <col className="machine-environment-name-column" />
                  <col className="machine-environment-path-column" />
                  <col className="machine-environment-created-column" />
                  <col className="provider-actions-column" />
                </colgroup>
                <thead>
                  <tr>
                    <th scope="col">Name</th>
                    <th scope="col">Path</th>
                    <th scope="col">Created at</th>
                    <th scope="col">
                      <span className="visually-hidden">Actions</span>
                    </th>
                  </tr>
                </thead>
                <tbody aria-live="polite" aria-busy={isEnvironmentsPending}>
                  {isEnvironmentsPending ? (
                    <EnvironmentTableMessage>
                      Loading environments...
                    </EnvironmentTableMessage>
                  ) : null}
                  {isEnvironmentsError ? (
                    <EnvironmentTableError
                      message={environmentsError.message}
                      onRetry={() => void refetchEnvironments()}
                    />
                  ) : null}
                  {!isEnvironmentsPending &&
                  !isEnvironmentsError &&
                  environments?.length === 0 ? (
                    <EnvironmentTableMessage>
                      No environments present
                    </EnvironmentTableMessage>
                  ) : null}
                  {!isEnvironmentsPending && !isEnvironmentsError
                    ? environments?.map((environment) => (
                        <EnvironmentRow
                          key={environment.environment_id}
                          environment={environment}
                          displayPath={formatEnvironmentPath(
                            environment,
                            machine?.descriptor.workspace_roots,
                            machine?.descriptor.path_convention,
                          )}
                          onDelete={() => setEnvironmentToDelete(environment)}
                          onUpdateName={() =>
                            setEnvironmentToRename(environment)
                          }
                        />
                      ))
                    : null}
                </tbody>
              </table>
            </div>
          </section>

          <AddEnvironmentDialog
            machineId={machineId}
            open={isAddDialogOpen}
            workspaceRootId={workspaceRoot?.id ?? ''}
            workspaceRootPath={workspaceRootPath}
            onClose={() => setIsAddDialogOpen(false)}
          />
          <DeleteEnvironmentDialog
            environment={environmentToDelete}
            onClose={() => setEnvironmentToDelete(null)}
          />
          {environmentToRename !== null ? (
            <UpdateEnvironmentNameDialog
              key={environmentToRename.environment_id}
              environment={environmentToRename}
              onClose={() => setEnvironmentToRename(null)}
            />
          ) : null}
        </div>
      </main>
    </div>
  )
}

function EnvironmentRow({
  displayPath,
  environment,
  onDelete,
  onUpdateName,
}: {
  displayPath: string
  environment: MachineEnvironment
  onDelete: () => void
  onUpdateName: () => void
}) {
  return (
    <tr>
      <td>
        <span className="provider-name">{environment.name}</span>
      </td>
      <td>
        <span className="provider-name" title={displayPath}>
          {displayPath}
        </span>
      </td>
      <td>
        <time
          className="provider-detail"
          dateTime={new Date(environment.created_at).toISOString()}
        >
          {formatDate(environment.created_at)}
        </time>
      </td>
      <td className="provider-actions-cell">
        <EnvironmentActionsMenu
          environment={environment}
          onDelete={onDelete}
          onUpdateName={onUpdateName}
        />
      </td>
    </tr>
  )
}

function EnvironmentTableMessage({ children }: { children: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={4}>
        {children}
      </td>
    </tr>
  )
}

function EnvironmentTableError({
  message,
  onRetry,
}: {
  message: string
  onRetry: () => void
}) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={4} title={message}>
        <span>Couldn&apos;t load environments</span>
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

function formatDate(value: number) {
  const date = new Date(value)
  return Number.isNaN(date.getTime())
    ? String(value)
    : DATE_FORMATTER.format(date)
}

function formatWorkspaceRootPath(
  uri: string | undefined,
  pathConvention: 'posix' | 'windows' | undefined,
) {
  if (uri === undefined) {
    return 'Workspace root /'
  }

  try {
    const url = new URL(uri)
    if (url.protocol !== 'file:') {
      return `${uri.replace(/\/$/, '')}/`
    }

    let path = decodeURIComponent(url.pathname)
    if (pathConvention === 'windows' && /^\/[a-zA-Z]:\//.test(path)) {
      path = path.slice(1)
    }
    return path.endsWith('/') ? path : `${path}/`
  } catch {
    return `${uri.replace(/\/$/, '')}/`
  }
}

function formatEnvironmentPath(
  environment: MachineEnvironment,
  roots: Array<{ id: string; uri: string }> | undefined,
  pathConvention: 'posix' | 'windows' | undefined,
) {
  const root = roots?.find(
    (candidate) => candidate.id === environment.workspace_root_id,
  )
  if (root === undefined) {
    return environment.path
  }

  const prefix = formatWorkspaceRootPath(root.uri, pathConvention)
  return environment.path === '.' ? prefix : `${prefix}${environment.path}`
}
