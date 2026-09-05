import { ArrowLeft01Icon, Folder03Icon, Edit02Icon, Settings01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { Link, NavLink, Outlet, useParams } from 'react-router-dom'
import { ApiError } from '../../lib/api-client'
import { useProjectEnvironments, useProjects } from './project-queries'
import { ProjectEnvironmentsTable } from './ProjectEnvironmentsTable'
import { ProjectRecentChats } from './ProjectRecentChats'

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

export function ProjectPage() {
  const { projectId = '' } = useParams()
  const projectPath = `/projects/${encodeURIComponent(projectId)}`
  return (
    <div className="provider-detail-shell">
      <aside className="provider-detail-sidebar project-chat-sidebar" aria-label="Project sidebar">
        <Link className="provider-detail-nav-item provider-detail-back" to="/projects" aria-label="Back to Dashboard" title="Back to Dashboard">
          <span className="nav-icon-frame" aria-hidden="true"><HugeiconsIcon icon={ArrowLeft01Icon} size={16} strokeWidth={1.5} /></span>
          <span className="nav-item-label">Back to Dashboard</span>
        </Link>
        <nav className="provider-detail-nav" aria-label="Project">
          <NavLink end to={projectPath} aria-label="New Chat" title="New Chat"
            className={({ isActive }) => `provider-detail-nav-item${isActive ? ' provider-detail-nav-item--active' : ''}`}>
            <span className="nav-icon-frame" aria-hidden="true"><HugeiconsIcon icon={Edit02Icon} size={16} strokeWidth={1.5} /></span>
            <span className="nav-item-label">New Chat</span>
          </NavLink>
          <NavLink to={`${projectPath}/environments`} aria-label="Environments" title="Environments"
            className={({ isActive }) => `provider-detail-nav-item${isActive ? ' provider-detail-nav-item--active' : ''}`}>
            <span className="nav-icon-frame" aria-hidden="true"><HugeiconsIcon icon={Folder03Icon} size={16} strokeWidth={1.5} /></span>
            <span className="nav-item-label">Environments</span>
          </NavLink>
        </nav>
        {UUID_PATTERN.test(projectId) ? <ProjectRecentChats key={projectId} projectId={projectId.toLowerCase()} /> : null}
        <nav className="project-sidebar-footer" aria-label="Project settings">
          <NavLink to={`${projectPath}/settings`} aria-label="Project Settings" title="Project Settings"
            className={({ isActive }) => `provider-detail-nav-item${isActive ? ' provider-detail-nav-item--active' : ''}`}>
            <span className="nav-icon-frame" aria-hidden="true"><HugeiconsIcon icon={Settings01Icon} size={16} strokeWidth={1.5} /></span>
            <span className="nav-item-label">Project Settings</span>
          </NavLink>
        </nav>
      </aside>
      <main className="provider-detail-main">
        <div className="provider-detail-container">
          {UUID_PATTERN.test(projectId)
            ? <Outlet />
            : <p className="project-detail-notice" role="alert">Invalid project ID.</p>}
        </div>
      </main>
    </div>
  )
}

export function ProjectSettingsPage() {
  return <header className="provider-detail-header">
    <h1 className="cursor-page-title">Settings</h1>
  </header>
}

export function ProjectEnvironmentsPage() {
  const { projectId = '' } = useParams()
  return <>
    <header className="provider-detail-header">
      <h1 className="cursor-page-title">Environments</h1>
    </header>
    <ProjectEnvironments key={projectId} projectId={projectId.toLowerCase()} />
  </>
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
