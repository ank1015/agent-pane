import {
  ArrowLeft01Icon,
  ArrowRight01Icon,
  AtIcon,
  Delete03Icon,
  Folder03Icon,
  Globe02Icon,
  ZapIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import {
  Link,
  Navigate,
  NavLink,
  matchPath,
  useLocation,
  useNavigate,
} from 'react-router-dom'
import { TableActionsMenu } from '../../components/TableActionsMenu'
import {
  type HarnessProviderModelOptions,
  useHarnessModelOptions,
} from '../harnesses/harness-queries'
import {
  DeleteProjectEnvironmentDialog,
  UpdateProjectEnvironmentNameDialog,
} from './ProjectEnvironmentDialogs'
import {
  type ProjectEnvironment,
  useProject,
  useProjectEnvironments,
} from './project-queries'
import {
  ProjectEnvironmentPromptComposer,
  type ProjectEnvironmentPromptSubmission,
} from './ProjectEnvironmentPromptComposer'
import { buildProjectConversation } from './project-conversation'
import { useProjectRunEventStream } from './project-run-event-stream'
import {
  createIdempotencyKey,
  useAbortProjectHarnessRun,
  useCreateProjectHarnessSession,
  useProjectHarnessRun,
  useProjectHarnessSession,
  useProjectRunEvents,
  useProjectSessionMessages,
  useProjectSessionRuns,
  useStartProjectHarnessRun,
} from './project-session-queries'
import type {
  ProjectRunSummary,
  RunEvent,
  SessionMessage,
} from './project-session-types'
import { isLiveRunStatus } from './project-session-types'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})
const ENVIRONMENT_HARNESS_ID = 'environment'
const EMPTY_REASONING_LEVELS: readonly string[] = []
const EMPTY_PROVIDER_MODEL_OPTIONS: readonly HarnessProviderModelOptions[] = []
const EMPTY_SESSION_MESSAGES: readonly SessionMessage[] = []
const EMPTY_SESSION_RUNS: readonly ProjectRunSummary[] = []
const EMPTY_RUN_EVENTS: readonly RunEvent[] = []
const ProjectEnvironmentConversation = lazy(() =>
  import('./ProjectEnvironmentConversation').then((module) => ({
    default: module.ProjectEnvironmentConversation,
  })),
)

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
  const environmentSessionMatch = matchPath(
    '/projects/:projectId/environments/sessions/:sessionId',
    pathname,
  )

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
          isCreateEnvironmentPage || environmentSessionMatch !== null
            ? ' project-create-main'
            : ''
        }`}
      >
        {isCreateEnvironmentPage ? (
          <CreateProjectEnvironmentPage
            projectId={projectId}
            environmentsPath={environmentsPath}
          />
        ) : environmentSessionMatch !== null ? (
          <ProjectEnvironmentSessionPage
            projectId={projectId}
            sessionId={environmentSessionMatch.params.sessionId ?? ''}
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
  const [environmentToRename, setEnvironmentToRename] =
    useState<ProjectEnvironment | null>(null)
  const [environmentToDelete, setEnvironmentToDelete] =
    useState<ProjectEnvironment | null>(null)
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
              <col className="project-environment-created-column" />
              <col className="provider-actions-column" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Host name</th>
                <th scope="col">Path</th>
                <th scope="col">Type</th>
                <th scope="col">Created at</th>
                <th scope="col">
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody aria-live="polite" aria-busy={environments.isPending}>
              <ProjectEnvironmentRows
                query={environments}
                onDelete={setEnvironmentToDelete}
                onUpdateName={setEnvironmentToRename}
              />
            </tbody>
          </table>
        </div>
      </section>

      {environmentToRename !== null ? (
        <UpdateProjectEnvironmentNameDialog
          key={environmentToRename.id}
          projectId={projectId}
          environment={environmentToRename}
          onClose={() => setEnvironmentToRename(null)}
        />
      ) : null}
      {environmentToDelete !== null ? (
        <DeleteProjectEnvironmentDialog
          key={environmentToDelete.id}
          projectId={projectId}
          environment={environmentToDelete}
          onClose={() => setEnvironmentToDelete(null)}
        />
      ) : null}
    </div>
  )
}

function ProjectEnvironmentRows({
  query,
  onDelete,
  onUpdateName,
}: {
  query: ReturnType<typeof useProjectEnvironments>
  onDelete: (environment: ProjectEnvironment) => void
  onUpdateName: (environment: ProjectEnvironment) => void
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
    <ProjectEnvironmentRow
      key={environment.id}
      environment={environment}
      onDelete={() => onDelete(environment)}
      onUpdateName={() => onUpdateName(environment)}
    />
  ))
}

function ProjectEnvironmentRow({
  environment,
  onDelete,
  onUpdateName,
}: {
  environment: ProjectEnvironment
  onDelete: () => void
  onUpdateName: () => void
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
        <time
          className="provider-detail"
          dateTime={formatDateTime(environment.created_at)}
        >
          {formatDate(environment.created_at)}
        </time>
      </td>
      <td className="provider-actions-cell">
        <TableActionsMenu
          label={`Actions for ${environment.name}`}
          actions={[
            {
              icon: AtIcon,
              label: 'Update name',
              onSelect: onUpdateName,
            },
            {
              danger: true,
              icon: Delete03Icon,
              label: 'Delete',
              onSelect: onDelete,
              separatorBefore: true,
            },
          ]}
        />
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
  projectId,
  environmentsPath,
}: {
  projectId: string
  environmentsPath: string
}) {
  const modelOptions = useHarnessModelOptions(ENVIRONMENT_HARNESS_ID)
  const createSession = useCreateProjectHarnessSession(projectId)
  const navigate = useNavigate()
  const retryRef = useRef<{ fingerprint: string; key: string } | null>(null)

  const submit = (submission: ProjectEnvironmentPromptSubmission) => {
    const request = {
      harness_id: ENVIRONMENT_HARNESS_ID,
      prompt: submission.prompt,
      attachments: [],
      config_override: {
        provider: submission.provider,
        model_id: submission.modelId,
        reasoning_level: submission.reasoningLevel,
        account_id: submission.accountId,
        project_id: projectId,
      },
    }
    const fingerprint = JSON.stringify(request)
    if (retryRef.current?.fingerprint !== fingerprint) {
      retryRef.current = { fingerprint, key: createIdempotencyKey() }
    }
    createSession.mutate(
      {
        idempotencyKey: retryRef.current.key,
        request,
      },
      {
        onSuccess: ({ session }) => {
          retryRef.current = null
          navigate(
            `/projects/${encodeURIComponent(projectId)}/environments/sessions/${encodeURIComponent(session.session_id)}`,
            { replace: true },
          )
        },
      },
    )
  }

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
        <h1 className="cursor-page-title project-environment-create-title">
          Create or Update Environments
        </h1>
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
          isSubmitting={createSession.isPending}
          submitError={createSession.isError ? createSession.error.message : null}
          onSubmit={submit}
        />
      </div>
    </div>
  )
}

function ProjectEnvironmentSessionPage({
  projectId,
  sessionId,
  environmentsPath,
}: {
  projectId: string
  sessionId: string
  environmentsPath: string
}) {
  const session = useProjectHarnessSession(projectId, sessionId)
  const messages = useProjectSessionMessages(projectId, sessionId)
  const runs = useProjectSessionRuns(projectId, sessionId)
  const modelOptions = useHarnessModelOptions(
    session.data?.harness_id ?? ENVIRONMENT_HARNESS_ID,
  )
  const sessionRun = session.data?.active_run ?? session.data?.latest_run ?? null
  const runId = sessionRun?.run_id ?? ''
  const run = useProjectHarnessRun(projectId, sessionId, runId)
  const events = useProjectRunEvents(projectId, sessionId, runId)
  const currentRun = run.data ?? sessionRun
  const latestEvent = events.data?.pages.at(-1)?.items.at(-1)
  const currentRunStatus = latestEvent?.run_status ?? currentRun?.status
  const stream = useProjectRunEventStream({
    projectId,
    sessionId,
    runId,
    runStatus: currentRunStatus,
    enabled:
      events.isSuccess &&
      currentRun !== null &&
      currentRunStatus !== undefined &&
      isLiveRunStatus(currentRunStatus),
  })
  const startRun = useStartProjectHarnessRun(projectId, sessionId)
  const abortRun = useAbortProjectHarnessRun(projectId, sessionId, runId)
  const startRunMutate = startRun.mutate
  const abortRunMutate = abortRun.mutate
  const refetchSession = session.refetch
  const refetchMessages = messages.refetch
  const refetchRuns = runs.refetch
  const refetchModelOptions = modelOptions.refetch
  const submitRetryRef = useRef<{ fingerprint: string; key: string } | null>(null)
  const abortRetryRef = useRef<{ stateVersion: number; id: string } | null>(null)
  const [composerVersion, setComposerVersion] = useState(0)
  const messageItems = useMemo(
    () =>
      messages.data?.pages.flatMap((page) => page.items) ??
      EMPTY_SESSION_MESSAGES,
    [messages.data],
  )
  const runItems = useMemo(
    () => runs.data?.pages.flatMap((page) => page.items) ?? EMPTY_SESSION_RUNS,
    [runs.data],
  )
  const runEvents = useMemo(
    () => events.data?.pages.flatMap((page) => page.items) ?? EMPTY_RUN_EVENTS,
    [events.data],
  )
  const eventsByRun = useMemo(() => {
    const result = new Map<string, readonly RunEvent[]>()
    if (runId.length > 0) result.set(runId, runEvents)
    return result
  }, [runEvents, runId])
  const conversation = useMemo(
    () => buildProjectConversation(messageItems, runItems, eventsByRun),
    [eventsByRun, messageItems, runItems],
  )
  const isRunLive =
    currentRunStatus !== undefined && isLiveRunStatus(currentRunStatus)
  const fetchNextMessagePage = messages.fetchNextPage
  const hasNextMessagePage = messages.hasNextPage
  const isFetchingNextMessagePage = messages.isFetchingNextPage
  const fetchNextRunPage = runs.fetchNextPage
  const hasNextRunPage = runs.hasNextPage
  const isFetchingNextRunPage = runs.isFetchingNextPage

  useEffect(() => {
    if (hasNextMessagePage && !isFetchingNextMessagePage) {
      void fetchNextMessagePage()
    }
  }, [
    fetchNextMessagePage,
    hasNextMessagePage,
    isFetchingNextMessagePage,
  ])

  useEffect(() => {
    if (hasNextRunPage && !isFetchingNextRunPage) {
      void fetchNextRunPage()
    }
  }, [fetchNextRunPage, hasNextRunPage, isFetchingNextRunPage])

  const submit = useCallback((submission: ProjectEnvironmentPromptSubmission) => {
    const currentRevision = session.data?.session.current_revision
    if (currentRevision === undefined || isRunLive) return
    const request = {
      prompt: submission.prompt,
      attachments: [],
      config_override: {
        provider: submission.provider,
        model_id: submission.modelId,
        reasoning_level: submission.reasoningLevel,
        account_id: submission.accountId,
        project_id: projectId,
      },
      expected_session_revision: currentRevision,
    }
    const fingerprint = JSON.stringify(request)
    if (submitRetryRef.current?.fingerprint !== fingerprint) {
      submitRetryRef.current = { fingerprint, key: createIdempotencyKey() }
    }
    startRunMutate(
      { idempotencyKey: submitRetryRef.current.key, request },
      {
        onSuccess: () => {
          submitRetryRef.current = null
          setComposerVersion((version) => version + 1)
        },
      },
    )
  }, [
    isRunLive,
    projectId,
    session.data?.session.current_revision,
    startRunMutate,
  ])

  const abort = useCallback(() => {
    if (currentRun === null || !isRunLive) return
    if (abortRetryRef.current?.stateVersion !== currentRun.state_version) {
      abortRetryRef.current = {
        stateVersion: currentRun.state_version,
        id: createIdempotencyKey(),
      }
    }
    abortRunMutate(
      {
        abort_id: abortRetryRef.current.id,
        expected_state_version: currentRun.state_version,
        reason: 'Stopped by user',
        payload: {},
      },
      { onSuccess: () => { abortRetryRef.current = null } },
    )
  }, [abortRunMutate, currentRun, isRunLive])

  const retryConversation = useCallback(() => {
    void Promise.all([refetchSession(), refetchMessages(), refetchRuns()])
  }, [refetchMessages, refetchRuns, refetchSession])
  const retryModelOptions = useCallback(() => {
    void refetchModelOptions()
  }, [refetchModelOptions])

  return (
    <div className="project-environment-create-page project-session-page">
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
            <span aria-current="page">Session</span>
          </li>
        </ol>
      </nav>
      <div className="project-session-workspace">
        <Suspense
          fallback={
            <div className="project-session-conversation-state" role="status">
              Loading conversation…
            </div>
          }
        >
          <ProjectEnvironmentConversation
            projectId={projectId}
            sessionId={sessionId}
            items={conversation}
            activeRunId={isRunLive ? runId : null}
            streamState={stream.state}
            isPending={session.isPending || messages.isPending || runs.isPending}
            isError={session.isError || messages.isError || runs.isError}
            onRetry={retryConversation}
            onAbortRun={abort}
            isAborting={abortRun.isPending}
          />
        </Suspense>
        <div className="project-session-composer-wrap">
          <ProjectEnvironmentPromptComposer
            key={composerVersion}
            providerAccounts={
              modelOptions.data?.providers ?? EMPTY_PROVIDER_MODEL_OPTIONS
            }
            reasoningLevels={
              modelOptions.data?.reasoning_levels ?? EMPTY_REASONING_LEVELS
            }
            isModelOptionsPending={modelOptions.isPending}
            isModelOptionsError={modelOptions.isError}
            onRetryModelOptions={retryModelOptions}
            isSubmitting={startRun.isPending || isRunLive}
            submitError={startRun.isError ? startRun.error.message : null}
            onSubmit={submit}
          />
          {isRunLive ? (
            <p className="project-session-composer-hint" role="status">
              {environmentRunStatusLabel(currentRunStatus, stream.state)}
            </p>
          ) : null}
        </div>
      </div>
    </div>
  )
}

function environmentRunStatusLabel(
  status: 'active' | 'waiting' | 'aborted' | 'completed' | 'failed' | undefined,
  streamState: ReturnType<typeof useProjectRunEventStream>['state'],
) {
  if (status === 'completed') return 'Environment run completed.'
  if (status === 'failed') return 'Environment run failed.'
  if (status === 'aborted') return 'Environment run stopped.'
  if (status === 'waiting') return 'Environment run is waiting.'
  if (streamState === 'reconnecting' || streamState === 'error') {
    return 'Reconnecting to environment progress…'
  }
  if (status === 'active') return 'Creating environment…'
  return 'Preparing environment session…'
}
