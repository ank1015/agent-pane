import {
  ArrowLeft01Icon,
  ArrowRight01Icon,
  Folder03Icon,
  Globe02Icon,
  ZapIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { Link, Navigate, NavLink, useLocation } from 'react-router-dom'
import {
  type HarnessProviderModelOptions,
  useHarnessModelOptions,
} from '../harnesses/harness-queries'
import {
  type ProjectEnvironment,
  useProject,
  useProjectEnvironments,
} from './project-queries'
import { ProjectEnvironmentPromptComposer } from './ProjectEnvironmentPromptComposer'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})
const ENVIRONMENT_HARNESS_ID = 'environment'
const EMPTY_REASONING_LEVELS: readonly string[] = []
const EMPTY_PROVIDER_MODEL_OPTIONS: readonly HarnessProviderModelOptions[] = []

const PROJECT_NAVIGATION = [
  { suffix: '/environments', label: 'Environments', icon: Folder03Icon },
  { suffix: '/triggers', label: 'Triggers', icon: ZapIcon },
  { suffix: '/sites', label: 'Sites', icon: Globe02Icon },
] as const

export function ProjectPage({ projectId }: { projectId: string }) {
  const projectPath = `/projects/${encodeURIComponent(projectId)}`
  const environmentsPath = `${projectPath}/environments`
  const createEnvironmentPath = `${environmentsPath}/create`
  const { pathname } = useLocation()
  const isProjectRoot = pathname === projectPath || pathname === `${projectPath}/`
  const isEnvironmentsPage =
    pathname === environmentsPath || pathname === `${environmentsPath}/`
  const isCreateEnvironmentPage =
    pathname === createEnvironmentPath ||
    pathname === `${createEnvironmentPath}/`

  if (isProjectRoot) {
    return <Navigate to={environmentsPath} replace />
  }

  return (
    <div className="cursor-shell machine-detail-shell">
      <aside className="cursor-sidebar dashboard-sidebar machine-detail-sidebar">
        <Link
          className="cursor-nav-item dashboard-nav-item machine-detail-back"
          to="/projects"
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
          aria-label="Project"
        >
          {PROJECT_NAVIGATION.map((item) => (
            <NavLink
              className={({ isActive }) =>
                `cursor-nav-item dashboard-nav-item machine-detail-nav-item${
                  isActive ? ' dashboard-nav-item--active' : ''
                }`
              }
              key={item.label}
              to={`${projectPath}${item.suffix}`}
            >
              <span className="nav-icon-frame" aria-hidden="true">
                <HugeiconsIcon
                  className="nav-item-icon"
                  icon={item.icon}
                  size={16}
                  color="currentColor"
                  strokeWidth={1.5}
                />
              </span>
              <span className="nav-item-label">{item.label}</span>
            </NavLink>
          ))}
        </nav>
      </aside>

      <main
        className={`cursor-main machine-detail-main${
          isCreateEnvironmentPage ? ' project-create-main' : ''
        }`}
      >
        {isCreateEnvironmentPage ? (
          <CreateProjectEnvironmentPage
            environmentsPath={environmentsPath}
          />
        ) : isEnvironmentsPage ? (
          <ProjectEnvironments projectId={projectId} />
        ) : null}
      </main>
    </div>
  )
}

function ProjectEnvironments({ projectId }: { projectId: string }) {
  const { data: project, isPending: isProjectPending } = useProject(projectId)
  const environments = useProjectEnvironments(projectId)
  const projectName =
    project?.name ?? (isProjectPending ? 'Loading…' : projectId)
  const createEnvironmentPath = `/projects/${encodeURIComponent(projectId)}/environments/create`

  return (
    <div className="cursor-container">
      <header className="page-header page-header--section">
        <h1 className="cursor-page-title">Environments</h1>
      </header>

      <section
        className="providers-section"
        aria-labelledby="configured-environments-title"
      >
        <div className="providers-section-header">
          <h2 id="configured-environments-title">
            {projectName} Configured Environments
          </h2>
          <Link
            className="cursor-button provider-add-button"
            to={createEnvironmentPath}
          >
            Create
          </Link>
        </div>

        <div className="providers-table-wrap">
          <table className="providers-table project-environments-table">
            <colgroup>
              <col className="project-environment-name-column" />
              <col className="project-environment-host-column" />
              <col className="project-environment-path-column" />
              <col className="project-environment-type-column" />
              <col className="project-environment-snapshot-column" />
              <col className="project-environment-created-column" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Host name</th>
                <th scope="col">Path</th>
                <th scope="col">Type</th>
                <th scope="col">Snapshot ID</th>
                <th scope="col">Created at</th>
              </tr>
            </thead>
            <tbody aria-live="polite" aria-busy={environments.isPending}>
              <ProjectEnvironmentRows query={environments} />
            </tbody>
          </table>
        </div>
      </section>
    </div>
  )
}

function ProjectEnvironmentRows({
  query,
}: {
  query: ReturnType<typeof useProjectEnvironments>
}) {
  if (query.isPending) {
    return <ProjectEnvironmentTableMessage message="Loading environments..." />
  }
  if (query.isError) {
    return (
      <tr>
        <td
          className="providers-table-message"
          colSpan={6}
          title={query.error.message}
        >
          <span>Couldn&apos;t load environments</span>
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
    return <ProjectEnvironmentTableMessage message="No environments present" />
  }

  return query.data.map((environment) => (
    <ProjectEnvironmentRow key={environment.id} environment={environment} />
  ))
}

function ProjectEnvironmentRow({
  environment,
}: {
  environment: ProjectEnvironment
}) {
  return (
    <tr>
      <td>
        <span className="provider-name" title={environment.name}>
          {environment.name}
        </span>
      </td>
      <td>
        <span className="provider-detail" title={environment.host_name}>
          {environment.host_name}
        </span>
      </td>
      <td>
        <span className="provider-detail" title={environment.path}>
          {environment.path}
        </span>
      </td>
      <td>
        <span className="provider-detail">
          {formatEnvironmentType(environment.type)}
        </span>
      </td>
      <td>
        <span className="provider-detail" title={environment.snapshot_id}>
          {environment.snapshot_id ?? '—'}
        </span>
      </td>
      <td>
        <time
          className="provider-detail"
          dateTime={formatDateTime(environment.created_at)}
        >
          {formatDate(environment.created_at)}
        </time>
      </td>
    </tr>
  )
}

function ProjectEnvironmentTableMessage({ message }: { message: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={6}>
        {message}
      </td>
    </tr>
  )
}

function formatEnvironmentType(type: ProjectEnvironment['type']) {
  return type === 'env' ? 'Environment' : 'Template'
}

function formatDate(value: number) {
  const date = new Date(value)
  return Number.isNaN(date.getTime())
    ? String(value)
    : DATE_FORMATTER.format(date)
}

function formatDateTime(value: number) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString()
}

function CreateProjectEnvironmentPage({
  environmentsPath,
}: {
  environmentsPath: string
}) {
  const modelOptions = useHarnessModelOptions(ENVIRONMENT_HARNESS_ID)

  return (
    <div className="project-environment-create-page">
      <nav className="project-breadcrumbs" aria-label="Breadcrumb">
        <ol>
          <li>
            <Link to={environmentsPath}>Environments</Link>
          </li>
          <li
            className="project-breadcrumb-separator"
            role="presentation"
            aria-hidden="true"
          >
            <HugeiconsIcon
              icon={ArrowRight01Icon}
              size={16}
              color="currentColor"
              strokeWidth={1.5}
            />
          </li>
          <li>
            <span aria-current="page">Create</span>
          </li>
        </ol>
      </nav>
      <div className="project-environment-composer-stage">
        <ProjectEnvironmentPromptComposer
          providerAccounts={
            modelOptions.data?.providers ?? EMPTY_PROVIDER_MODEL_OPTIONS
          }
          reasoningLevels={
            modelOptions.data?.reasoning_levels ?? EMPTY_REASONING_LEVELS
          }
          isModelOptionsPending={modelOptions.isPending}
          isModelOptionsError={modelOptions.isError}
          onRetryModelOptions={() => void modelOptions.refetch()}
        />
      </div>
    </div>
  )
}
