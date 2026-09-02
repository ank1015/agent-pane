import {
  Archive03Icon,
  ArrowLeft01Icon,
  AtIcon,
  CloudIcon,
  ComputerIcon,
  Delete03Icon,
  Folder03Icon,
  Globe02Icon,
  Loading03Icon,
  MoreHorizontalIcon,
  SquarePenIcon,
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
  type MachineEnvironment,
  useMaterializeSandboxEnvironmentTemplate,
} from '../machines/machine-queries'
import {
  DeleteProjectEnvironmentDialog,
  UpdateProjectEnvironmentNameDialog,
} from './ProjectEnvironmentDialogs'
import {
  type ProjectBootstrapHarness,
  type ProjectEnvironment,
  harnessModelOptions,
  useProject,
  useProjectBootstrap,
  useProjectEnvironments,
} from './project-queries'
import {
  ProjectEnvironmentPromptComposer,
  type ProjectEnvironmentPromptLockedOptions,
  type ProjectEnvironmentPromptSubmission,
} from './ProjectEnvironmentPromptComposer'
import { ProjectEnvironmentSelector } from './ProjectEnvironmentSelector'
import { ProjectHarnessSelector } from './ProjectHarnessSelector'
import { harnessUiDefinition } from './project-harness-ui'
import { buildProjectConversation } from './project-conversation'
import { useProjectRunEventStream } from './project-run-event-stream'
import {
  createIdempotencyKey,
  useAbortProjectHarnessRun,
  useArchiveProjectSession,
  useCreateProjectHarnessSession,
  useProjectHarnessRun,
  useProjectHarnessSession,
  useProjectRunEvents,
  useProjectSessions,
  useProjectSessionMessages,
  useProjectSessionRuns,
  useStartProjectHarnessRun,
} from './project-session-queries'
import type {
  ProjectSession,
  ProjectRunSummary,
  RunEventType,
  SessionMessage,
} from './project-session-types'
import { isLiveRunStatus } from './project-session-types'
import type { ProviderKind } from '../providers/provider-queries'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})
const ENVIRONMENT_HARNESS_ID = 'environment'
const EMPTY_REASONING_LEVELS: readonly string[] = []
const EMPTY_PROVIDER_MODEL_OPTIONS: readonly HarnessProviderModelOptions[] = []
const EMPTY_PROJECT_BOOTSTRAP_HARNESSES: readonly ProjectBootstrapHarness[] = []
const EMPTY_PROJECT_ENVIRONMENTS: readonly ProjectEnvironment[] = []
const EMPTY_SESSION_MESSAGES: readonly SessionMessage[] = []
const EMPTY_SESSION_RUNS: readonly ProjectRunSummary[] = []
const PROVIDER_KINDS = new Set<ProviderKind>([
  'anthropic',
  'chatgpt',
  'deepseek',
  'fireworks',
  'openai',
  'openrouter',
])

type HarnessExecutionTarget = {
  machine_id: string
  workspace_root_id: string
  cwd: string
}

type LandingSessionRetry = {
  fingerprint: string
  idempotencyKey: string
  execution: HarnessExecutionTarget | null
}

function sessionPromptLockedOptions(
  session: ProjectSession | undefined,
): ProjectEnvironmentPromptLockedOptions | undefined {
  if (session === undefined) return undefined

  const accountId = stringConfigValue(session.harness_config, 'account_id')
  const provider = stringConfigValue(session.harness_config, 'provider')
  const modelId = stringConfigValue(session.harness_config, 'model_id')
  const reasoningLevel = stringConfigValue(
    session.harness_config,
    'reasoning_level',
  )
  if (
    accountId === null ||
    !isProviderKind(provider) ||
    modelId === null ||
    reasoningLevel === null
  ) {
    return undefined
  }

  return {
    accountId,
    provider,
    modelId,
    reasoningLevel,
    webSearchEnabled: session.web_search_enabled,
  }
}

function stringConfigValue(
  config: Readonly<Record<string, unknown>>,
  key: string,
) {
  const value = config[key]
  return typeof value === 'string' && value.length > 0 ? value : null
}

function isProviderKind(value: string | null): value is ProviderKind {
  return value !== null && PROVIDER_KINDS.has(value as ProviderKind)
}

function ProjectSessionContext({
  harnessId,
  environments,
}: {
  harnessId: string
  environments: readonly ProjectEnvironment[]
}) {
  const showsEnvironments =
    harnessUiDefinition(harnessId).environmentSelection !== 'none'

  return (
    <div className="project-session-context">
      <span
        className="project-context-picker-trigger project-context-picker-trigger--static"
        aria-label={`Harness: ${harnessId}`}
      >
        <span>{harnessId}</span>
      </span>
      {showsEnvironments
        ? environments.map((environment) => (
            <span
              key={environment.id}
              className="project-context-picker-trigger project-context-picker-trigger--static"
              aria-label={`Environment: ${environment.name}`}
              title={environment.name}
            >
              <HugeiconsIcon
                className="project-context-picker-trigger-icon"
                icon={environment.type === 'env' ? ComputerIcon : CloudIcon}
                size={13}
                strokeWidth={1.5}
                aria-hidden="true"
              />
              <span>{environment.name}</span>
            </span>
          ))
        : null}
    </div>
  )
}

function executionTargetFromProjectEnvironment(
  environment: ProjectEnvironment,
): HarnessExecutionTarget {
  if (
    environment.machine_id === undefined ||
    environment.workspace_root_id === undefined
  ) {
    throw new Error('The selected environment is missing execution details.')
  }
  return {
    machine_id: environment.machine_id,
    workspace_root_id: environment.workspace_root_id,
    cwd: environment.path,
  }
}

function executionTargetFromEnvironment(
  environment: MachineEnvironment,
): HarnessExecutionTarget {
  return {
    machine_id: environment.machine_id,
    workspace_root_id: environment.workspace_root_id,
    cwd: environment.path,
  }
}

function buildHarnessConfig({
  harnessId,
  projectId,
  submission,
  execution,
}: {
  harnessId: string
  projectId: string
  submission: ProjectEnvironmentPromptSubmission
  execution: HarnessExecutionTarget | null
}) {
  const model = {
    provider: submission.provider,
    model_id: submission.modelId,
    reasoning_level: submission.reasoningLevel,
    account_id: submission.accountId,
    web_search_enabled: submission.webSearchEnabled,
  }
  if (harnessId === ENVIRONMENT_HARNESS_ID) {
    return { ...model, project_id: projectId }
  }
  if (harnessId === 'codex' || harnessId === 'pi') {
    if (execution === null) {
      throw new Error(`${harnessId} requires an execution environment.`)
    }
    return {
      ...model,
      execution,
      is_replaced: false,
    }
  }
  return execution === null ? model : { ...model, execution }
}

function isSessionId(value: string | undefined) {
  return value !== undefined &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      value,
    )
}

const ProjectEnvironmentConversation = lazy(() =>
  import('./ProjectEnvironmentConversation').then((module) => ({
    default: module.ProjectEnvironmentConversation,
  })),
)

const ProjectRunDetailsDrawer = lazy(() =>
  import('./ProjectRunDetailsDrawer').then((module) => ({
    default: module.ProjectRunDetailsDrawer,
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
  const { pathname } = useLocation()
  const navigate = useNavigate()
  const sessions = useProjectSessions(projectId)
  const archiveSession = useArchiveProjectSession(projectId)
  const isProjectRoot = pathname === projectPath || pathname === `${projectPath}/`
  const isEnvironmentsPage =
    pathname === environmentsPath || pathname === `${environmentsPath}/`
  const possibleProjectSessionMatch = matchPath(
    '/projects/:projectId/:sessionId',
    pathname,
  )
  const projectSessionMatch = isSessionId(
    possibleProjectSessionMatch?.params.sessionId,
  )
    ? possibleProjectSessionMatch
    : null

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

          <div className="project-recents">
            <div className="project-recents-header">
              <span className="project-recents-label">Recents</span>

              <span className="project-recents-actions">
                <button
                  type="button"
                  className="project-recents-action"
                  aria-label="Recent options"
                  title="Recent options"
                >
                  <HugeiconsIcon
                    icon={MoreHorizontalIcon}
                    size={16}
                    color="currentColor"
                    strokeWidth={1.5}
                    aria-hidden="true"
                  />
                </button>
                <Link
                  className="project-recents-action"
                  to={projectPath}
                  aria-label="New chat"
                  title="New chat"
                >
                  <HugeiconsIcon
                    icon={SquarePenIcon}
                    size={16}
                    color="currentColor"
                    strokeWidth={1.5}
                    aria-hidden="true"
                  />
                </Link>
              </span>
            </div>

            <div className="project-session-list" aria-label="Recent sessions">
              {sessions.isPending ? (
                <div className="project-session-list-state">Loading sessions…</div>
              ) : sessions.isError ? (
                <div className="project-session-list-state">
                  Couldn’t load sessions
                </div>
              ) : (
                sessions.data.map((session) => {
                  const sessionPath = `${projectPath}/${encodeURIComponent(session.id)}`
                  const isSelected =
                    pathname === sessionPath || pathname === `${sessionPath}/`
                  const isArchiving =
                    archiveSession.isPending &&
                    archiveSession.variables === session.id

                  return (
                    <div
                      className={`project-session-item${
                        isSelected ? ' project-session-item--active' : ''
                      }${
                        session.is_active
                          ? ' project-session-item--running'
                          : ''
                      }`}
                      key={session.id}
                    >
                      <NavLink
                        className="project-session-link"
                        to={sessionPath}
                        title={session.title}
                      >
                        <span className="project-session-name">
                          {session.title}
                        </span>
                      </NavLink>

                      <span className="project-session-trailing">
                        {session.is_active ? (
                          <HugeiconsIcon
                            className="project-session-spinner"
                            icon={Loading03Icon}
                            size={15}
                            color="currentColor"
                            strokeWidth={1.6}
                            aria-label="Session is running"
                          />
                        ) : (
                          <button
                            type="button"
                            className="project-session-archive"
                            aria-label={`Archive ${session.title}`}
                            title="Archive"
                            disabled={isArchiving}
                            onClick={() => {
                              archiveSession.mutate(session.id, {
                                onSuccess: () => {
                                  if (isSelected) {
                                    navigate(projectPath)
                                  }
                                },
                              })
                            }}
                          >
                            <HugeiconsIcon
                              className={
                                isArchiving ? 'project-session-spinner' : undefined
                              }
                              icon={
                                isArchiving ? Loading03Icon : Archive03Icon
                              }
                              size={15}
                              color="currentColor"
                              strokeWidth={1.5}
                              aria-hidden="true"
                            />
                          </button>
                        )}
                      </span>
                    </div>
                  )
                })
              )}
            </div>
          </div>
        </nav>
      </aside>

      <main
        className={`cursor-main machine-detail-main${
          projectSessionMatch !== null
            ? ' project-create-main'
            : ''
        }`}
      >
        {isProjectRoot ? (
          <ProjectLandingPage projectId={projectId} />
        ) : isEnvironmentsPage ? (
          <ProjectEnvironments projectId={projectId} />
        ) : projectSessionMatch !== null ? (
          <ProjectSessionPage
            projectId={projectId}
            sessionId={projectSessionMatch.params.sessionId ?? ''}
          />
        ) : null}
      </main>
    </div>
  )
}

function ProjectLandingPage({ projectId }: { projectId: string }) {
  const bootstrap = useProjectBootstrap(projectId)
  const createSession = useCreateProjectHarnessSession(projectId)
  const materializeTemplate = useMaterializeSandboxEnvironmentTemplate()
  const navigate = useNavigate()
  const retryRef = useRef<LandingSessionRetry | null>(null)
  const [submissionError, setSubmissionError] = useState<string | null>(null)
  const [selectedHarnessId, setSelectedHarnessId] = useState<string | null>(
    null,
  )
  const [selectedEnvironmentId, setSelectedEnvironmentId] = useState<
    string | null
  >(null)
  const harnesses =
    bootstrap.data?.harnesses ?? EMPTY_PROJECT_BOOTSTRAP_HARNESSES
  const environments =
    bootstrap.data?.project_environments ?? EMPTY_PROJECT_ENVIRONMENTS
  const resolvedHarnessId =
    selectedHarnessId !== null &&
    harnesses.some((harness) => harness.harness_id === selectedHarnessId)
      ? selectedHarnessId
      : (harnesses.find(
          (harness) => harness.harness_id === ENVIRONMENT_HARNESS_ID,
        )?.harness_id ??
        harnesses[0]?.harness_id ??
        null)
  const resolvedEnvironmentId =
    selectedEnvironmentId !== null &&
    environments.some(
      (environment) => environment.id === selectedEnvironmentId,
    )
      ? selectedEnvironmentId
      : (environments[0]?.id ?? null)
  const harnessUi = harnessUiDefinition(resolvedHarnessId)
  const selectedEnvironment =
    environments.find(
      (environment) => environment.id === resolvedEnvironmentId,
    ) ?? null
  const modelOptions =
    bootstrap.data === undefined || resolvedHarnessId === null
      ? null
      : harnessModelOptions(bootstrap.data, resolvedHarnessId)

  const submit = async (submission: ProjectEnvironmentPromptSubmission) => {
    if (resolvedHarnessId === null) return
    setSubmissionError(null)
    const environmentId =
      harnessUi.environmentSelection === 'single'
        ? selectedEnvironment?.id ?? null
        : null
    const fingerprint = JSON.stringify({
      harnessId: resolvedHarnessId,
      environmentId,
      submission,
    })
    if (retryRef.current?.fingerprint !== fingerprint) {
      retryRef.current = {
        fingerprint,
        idempotencyKey: createIdempotencyKey(),
        execution: null,
      }
    }
    const retry = retryRef.current

    try {
      let execution = retry.execution
      if (harnessUi.environmentSelection === 'single') {
        if (selectedEnvironment === null) {
          throw new Error('Select an environment before starting the session.')
        }
        if (execution === null) {
          execution =
            selectedEnvironment.type === 'template'
              ? executionTargetFromEnvironment(
                  (
                    await materializeTemplate.mutateAsync(
                      selectedEnvironment.id,
                    )
                  ).environment,
                )
              : executionTargetFromProjectEnvironment(selectedEnvironment)
          retry.execution = execution
        }
      }

      const response = await createSession.mutateAsync({
        idempotencyKey: retry.idempotencyKey,
        request: {
          harness_id: resolvedHarnessId,
          prompt: submission.prompt,
          attachments: [],
          environment_ids: environmentId === null ? [] : [environmentId],
          config_override: buildHarnessConfig({
            harnessId: resolvedHarnessId,
            projectId,
            submission,
            execution,
          }),
        },
      })
      retryRef.current = null
      navigate(
        `/projects/${encodeURIComponent(projectId)}/${encodeURIComponent(response.session.id)}`,
      )
    } catch (error) {
      setSubmissionError(
        error instanceof Error ? error.message : 'Could not start the session.',
      )
    }
  }

  return (
    <div className="project-landing-page">
      <div className="project-landing-composer-stack">
        <div className="project-landing-context-row">
          <ProjectHarnessSelector
            harnesses={harnesses}
            selectedHarnessId={resolvedHarnessId}
            isPending={bootstrap.isPending}
            onSelect={setSelectedHarnessId}
          />
          {harnessUi.environmentSelection === 'single' ? (
            <ProjectEnvironmentSelector
              environments={environments}
              selectedEnvironmentId={resolvedEnvironmentId}
              isPending={bootstrap.isPending}
              onSelect={setSelectedEnvironmentId}
            />
          ) : null}
        </div>
        <ProjectEnvironmentPromptComposer
          providerAccounts={
            modelOptions?.providers ?? EMPTY_PROVIDER_MODEL_OPTIONS
          }
          reasoningLevels={
            modelOptions?.reasoning_levels ?? EMPTY_REASONING_LEVELS
          }
          isModelOptionsPending={bootstrap.isPending}
          isModelOptionsError={bootstrap.isError}
          onRetryModelOptions={() => void bootstrap.refetch()}
          isSubmissionReady={
            resolvedHarnessId !== null &&
            (harnessUi.environmentSelection !== 'single' ||
              selectedEnvironment !== null)
          }
          isSubmitting={
            createSession.isPending || materializeTemplate.isPending
          }
          submitError={submissionError}
          onSubmit={(submission) => void submit(submission)}
        />
      </div>
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

function ProjectSessionPage({
  projectId,
  sessionId,
}: {
  projectId: string
  sessionId: string
}) {
  const session = useProjectHarnessSession(projectId, sessionId)
  const messages = useProjectSessionMessages(projectId, sessionId)
  const runs = useProjectSessionRuns(projectId, sessionId)
  const modelOptions = useHarnessModelOptions(
    session.data?.session.harness_id ?? ENVIRONMENT_HARNESS_ID,
  )
  const sessionRecord = session.data?.session
  const lockedOptions = useMemo(
    () => sessionPromptLockedOptions(sessionRecord),
    [sessionRecord],
  )
  const sessionRun = session.data?.active_run ?? session.data?.latest_run ?? null
  const runId = sessionRun?.run_id ?? ''
  const run = useProjectHarnessRun(projectId, sessionId, runId)
  const events = useProjectRunEvents(projectId, sessionId, runId)
  const currentRun = run.data ?? sessionRun
  const latestEvent = events.data?.pages.at(-1)?.items.at(-1)
  const currentRunStatus = latestEvent?.run_status ?? currentRun?.status
  useProjectRunEventStream({
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
  const [composerVersion, setComposerVersion] = useState(0)
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null)
  const [isRunDrawerClosing, setIsRunDrawerClosing] = useState(false)
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
  const conversation = useMemo(
    () => buildProjectConversation(messageItems, runItems),
    [messageItems, runItems],
  )
  const isRunLive =
    currentRunStatus !== undefined && isLiveRunStatus(currentRunStatus)
  const selectedRun =
    selectedRunId === currentRun?.run_id
      ? currentRun
      : (runItems.find((candidate) => candidate.run_id === selectedRunId) ?? null)
  const transcriptRefreshSequence = useMemo(() => {
    if (!isRunLive || selectedRunId !== runId) return null
    const transcriptEvents =
      events.data?.pages
        .flatMap((page) => page.items)
        .filter((event) => shouldRefreshRunTranscript(event.type)) ?? []
    return transcriptEvents.at(-1)?.sequence ?? null
  }, [events.data, isRunLive, runId, selectedRunId])
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
    session.data?.session.current_revision,
    startRunMutate,
  ])

  const stopRun = useCallback(() => {
    const stateVersion = latestEvent?.state_version ?? currentRun?.state_version
    if (
      !isRunLive ||
      runId.length === 0 ||
      stateVersion === undefined ||
      abortRun.isPending
    ) {
      return
    }
    abortRunMutate({
      abort_id: createIdempotencyKey(),
      expected_state_version: stateVersion,
      reason: 'user_requested',
    })
  }, [
    abortRun.isPending,
    abortRunMutate,
    currentRun?.state_version,
    isRunLive,
    latestEvent?.state_version,
    runId,
  ])

  const retryConversation = useCallback(() => {
    void Promise.all([refetchSession(), refetchMessages(), refetchRuns()])
  }, [refetchMessages, refetchRuns, refetchSession])
  const retryModelOptions = useCallback(() => {
    void refetchModelOptions()
  }, [refetchModelOptions])
  const openRunDetails = useCallback((selectedId: string) => {
    setIsRunDrawerClosing(false)
    setSelectedRunId(selectedId)
  }, [])
  const closeRunDetails = useCallback(() => {
    setIsRunDrawerClosing(true)
  }, [])
  const finishClosingRunDetails = useCallback(() => {
    setSelectedRunId(null)
    setIsRunDrawerClosing(false)
  }, [])

  return (
    <div className="project-session-page">
      <div className="project-session-workspace">
        <header className="project-session-header">
          {sessionRecord === undefined ? null : (
            <ProjectSessionContext
              harnessId={sessionRecord.harness_id}
              environments={sessionRecord.environments}
            />
          )}
        </header>
        <div className="project-session-body">
          <div className="project-session-main-pane">
            <Suspense
              fallback={
                <div className="project-session-conversation-state" role="status">
                  Loading conversation…
                </div>
              }
            >
              <ProjectEnvironmentConversation
                key={`${projectId}:${sessionId}`}
                sessionKey={`${projectId}:${sessionId}`}
                items={conversation}
                isPending={session.isPending || messages.isPending || runs.isPending}
                isError={session.isError || messages.isError || runs.isError}
                onRetry={retryConversation}
                onOpenRunDetails={openRunDetails}
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
                lockedOptions={lockedOptions}
                optionsReadOnly
                isModelOptionsPending={modelOptions.isPending}
                isModelOptionsError={modelOptions.isError}
                onRetryModelOptions={retryModelOptions}
                isSubmissionReady={lockedOptions !== undefined}
                isSubmitting={startRun.isPending}
                isRunActive={isRunLive}
                isStopping={abortRun.isPending}
                submitError={
                  startRun.isError
                    ? startRun.error.message
                    : abortRun.isError
                      ? abortRun.error.message
                      : null
                }
                onSubmit={submit}
                onStop={stopRun}
              />
            </div>
          </div>
        </div>
        {selectedRun === null ? null : (
          <div
            className={`project-run-drawer-slot${isRunDrawerClosing ? ' project-run-drawer-slot--closing' : ''}`}
          >
            <Suspense fallback={null}>
              <ProjectRunDetailsDrawer
                projectId={projectId}
                sessionId={sessionId}
                run={selectedRun}
                refreshSequence={transcriptRefreshSequence}
                isClosing={isRunDrawerClosing}
                onClose={closeRunDetails}
                onClosed={finishClosingRunDetails}
              />
            </Suspense>
          </div>
        )}
      </div>
    </div>
  )
}

function shouldRefreshRunTranscript(type: RunEventType) {
  return (
    type === 'turn_ended' ||
    type === 'run_waiting' ||
    type === 'run_completed' ||
    type === 'run_failed' ||
    type === 'run_aborted'
  )
}
