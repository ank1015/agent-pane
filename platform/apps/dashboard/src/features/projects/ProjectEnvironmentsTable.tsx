import { MoreHorizontalIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type { ReactNode } from 'react'
import type { useProjectEnvironments } from './project-queries'
import type { ProjectEnvironment } from './project-types'

export function ProjectEnvironmentsTable({ query }: { query: ReturnType<typeof useProjectEnvironments> }) {
  return (
    <div className="providers-table-wrap" role="region" aria-label="Project environments" tabIndex={0}>
      <table className="providers-table project-environments-table">
        <caption className="visually-hidden">Saved environments for this project</caption>
        <colgroup>
          <col style={{ width: '25%' }} /><col style={{ width: '12%' }} />
          <col style={{ width: '36%' }} /><col /><col className="provider-actions-column" />
        </colgroup>
        <thead><tr>
          <th scope="col">Name</th><th scope="col">Type</th>
          <th scope="col">Workspace root</th><th scope="col">Path</th><th scope="col"><span className="visually-hidden">Actions</span></th>
        </tr></thead>
        <tbody aria-live="polite" aria-busy={query.isFetching}>
          {query.isPending ? <TableMessage>{query.fetchStatus === 'paused' ? 'You’re offline. Waiting to load environments…' : 'Loading environments…'}</TableMessage> : null}
          {query.isError && !query.data ? <TableMessage>
            <span role="alert">Couldn’t load environments. {query.error.message}</span>
            <button type="button" className="providers-retry-button" disabled={query.isFetching} onClick={() => void query.refetch()}>Retry</button>
          </TableMessage> : null}
          {query.data?.length === 0 ? <TableMessage>No environments present</TableMessage> : null}
          {query.data?.map((environment) => <EnvironmentRow key={environment.id} environment={environment} />)}
        </tbody>
      </table>
    </div>
  )
}

function EnvironmentRow({ environment }: { environment: ProjectEnvironment }) {
  return (
    <tr>
      <td><span className="provider-name" title={environment.name}>{environment.name}</span></td>
      <td><span className="provider-detail">{environment.type === 'machine' ? 'Machine' : 'Sandbox'}</span></td>
      <td><span className="provider-detail environment-code environment-root-path" title={environment.workspace_root_path ?? 'Root path not yet known'}>{environment.workspace_root_path ?? 'Not yet known'}</span></td>
      <td><span className="provider-detail environment-code" title={environment.path}>{environment.path}</span></td>
      <td className="provider-actions-cell">
        <button
          type="button"
          className="cursor-button cursor-button--ghost cursor-icon-button provider-actions-trigger"
          aria-label={`Actions for ${environment.name}`}
          disabled
          title="Environment actions are not available yet"
        >
          <HugeiconsIcon icon={MoreHorizontalIcon} size={16} color="currentColor" strokeWidth={1.5} aria-hidden="true" />
        </button>
      </td>
    </tr>
  )
}

function TableMessage({ children }: { children: ReactNode }) {
  return <tr><td className="providers-table-message" colSpan={5}>{children}</td></tr>
}
