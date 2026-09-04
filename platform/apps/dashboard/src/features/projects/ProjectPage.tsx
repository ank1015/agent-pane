import { ArrowLeft01Icon, Folder03Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { Link, NavLink, useParams } from 'react-router-dom'
import { ApiError } from '../../lib/api-client'
import { useProjectEnvironments, useProjects } from './project-queries'
import { ProjectEnvironmentsTable } from './ProjectEnvironmentsTable'

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

export function ProjectPage() {
  const { projectId = '' } = useParams()
  return (
    <div className="provider-detail-shell">
      <aside className="provider-detail-sidebar">
        <Link className="provider-detail-nav-item provider-detail-back" to="/projects" aria-label="Back to Dashboard" title="Back to Dashboard">
          <span className="nav-icon-frame" aria-hidden="true"><HugeiconsIcon icon={ArrowLeft01Icon} size={16} strokeWidth={1.5} /></span>
          <span className="nav-item-label">Back to Dashboard</span>
        </Link>
        <nav className="provider-detail-nav" aria-label="Project">
          <NavLink end to={`/projects/${encodeURIComponent(projectId)}`} aria-label="Environments" title="Environments"
            className={({ isActive }) => `provider-detail-nav-item${isActive ? ' provider-detail-nav-item--active' : ''}`}>
            <span className="nav-icon-frame" aria-hidden="true"><HugeiconsIcon icon={Folder03Icon} size={16} strokeWidth={1.5} /></span>
            <span className="nav-item-label">Environments</span>
          </NavLink>
        </nav>
      </aside>
      <main className="provider-detail-main">
        <div className="provider-detail-container">
          <header className="provider-detail-header"><h1 className="cursor-page-title">Environments</h1></header>
          {UUID_PATTERN.test(projectId)
            ? <ProjectEnvironments key={projectId} projectId={projectId.toLowerCase()} />
            : <p className="project-detail-notice" role="alert">Invalid project ID.</p>}
        </div>
      </main>
    </div>
  )
}

function ProjectEnvironments({ projectId }: { projectId: string }) {
  // The existing list cache supplies the project name, including on direct navigation.
  // Both independent requests start together; environment reads never need a gateway call.
  const projects = useProjects()
  const environments = useProjectEnvironments(projectId)
  const project = projects.data?.find((item) => item.id === projectId)

  if (environments.error instanceof ApiError && environments.error.status === 404) {
    return <p className="project-detail-notice" role="alert">Project not found. It may have been deleted.</p>
  }

  return (
    <section className="providers-section" aria-labelledby="project-environments-title">
      <div className="providers-section-header">
        <h2 id="project-environments-title">{project ? `${project.name} · ` : ''}Configured Environments</h2>
      </div>
      {projects.isError && !project ? <p className="project-detail-notice" role="status">Couldn’t load the project name. <button className="providers-retry-button" type="button" onClick={() => void projects.refetch()}>Retry</button></p> : null}
      {environments.isError && environments.data ? (
        <div className="projects-refresh-warning" role="status">
          <span>Couldn’t refresh environments. Showing the last saved list.</span>
          <button className="providers-retry-button" type="button" disabled={environments.isFetching} onClick={() => void environments.refetch()}>Retry</button>
        </div>
      ) : null}
      <ProjectEnvironmentsTable query={environments} />
    </section>
  )
}
