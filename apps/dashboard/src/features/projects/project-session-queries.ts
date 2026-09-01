import {
  type InfiniteData,
  type QueryClient,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { getJson, postJson } from '../../lib/api-client'
import { projectKeys } from './project-queries'
import {
  type AbortProjectHarnessRunRequest,
  type CreateProjectHarnessSessionRequest,
  type CreateProjectHarnessSessionResponse,
  type ProjectHarnessSession,
  type ProjectRun,
  type ProjectRunPage,
  type ProjectRunSummary,
  type RunAbortResult,
  type RunEvent,
  type RunEventPage,
  type RunStatus,
  type SessionMessage,
  type SessionMessagePage,
  type StartProjectHarnessRunRequest,
  type StartProjectHarnessRunResponse,
  isLiveRunStatus,
  isTerminalRunStatus,
} from './project-session-types'

const MESSAGE_PAGE_SIZE = 100
const RUN_PAGE_SIZE = 100
const EVENT_PAGE_SIZE = 500

export const projectSessionKeys = {
  all: (projectId: string) =>
    [...projectKeys.detail(projectId), 'harness-sessions'] as const,
  detail: (projectId: string, sessionId: string) =>
    [...projectSessionKeys.all(projectId), 'detail', sessionId] as const,
  messages: (projectId: string, sessionId: string) =>
    [...projectSessionKeys.detail(projectId, sessionId), 'messages'] as const,
  runsRoot: (projectId: string, sessionId: string) =>
    [...projectSessionKeys.detail(projectId, sessionId), 'runs'] as const,
  runs: (projectId: string, sessionId: string, status?: RunStatus) =>
    [...projectSessionKeys.runsRoot(projectId, sessionId), status ?? 'all'] as const,
  run: (projectId: string, sessionId: string, runId: string) =>
    [...projectSessionKeys.detail(projectId, sessionId), 'run', runId] as const,
  events: (projectId: string, sessionId: string, runId: string) =>
    [...projectSessionKeys.run(projectId, sessionId, runId), 'events'] as const,
}

export type CreateProjectHarnessSessionVariables = {
  idempotencyKey: string
  request: CreateProjectHarnessSessionRequest
}

export type StartProjectHarnessRunVariables = {
  idempotencyKey: string
  request: StartProjectHarnessRunRequest
}

export function createIdempotencyKey() {
  return crypto.randomUUID()
}

export function useCreateProjectHarnessSession(projectId: string) {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ idempotencyKey, request }: CreateProjectHarnessSessionVariables) =>
      postJson<CreateProjectHarnessSessionResponse>(
        `${projectSessionsEndpoint(projectId)}`,
        request,
        { headers: { 'Idempotency-Key': idempotencyKey } },
      ),
    onSuccess: (response, variables) => {
      seedAcceptedRun(
        queryClient,
        projectId,
        response,
        response.session,
        variables.request.harness_id,
      )
    },
  })
}

export function useProjectHarnessSession(
  projectId: string,
  sessionId: string,
) {
  return useQuery({
    queryKey: projectSessionKeys.detail(projectId, sessionId),
    queryFn: ({ signal }) =>
      getJson<ProjectHarnessSession>(
        projectSessionEndpoint(projectId, sessionId),
        signal,
      ),
    enabled: projectId.length > 0 && sessionId.length > 0,
  })
}

export function useProjectSessionMessages(
  projectId: string,
  sessionId: string,
) {
  return useInfiniteQuery({
    queryKey: projectSessionKeys.messages(projectId, sessionId),
    queryFn: ({ pageParam, signal }) =>
      getJson<SessionMessagePage>(
        withSearchParams(
          `${projectSessionEndpoint(projectId, sessionId)}/messages`,
          {
            after_revision: pageParam > 0 ? pageParam : undefined,
            limit: MESSAGE_PAGE_SIZE,
          },
        ),
        signal,
      ),
    initialPageParam: 0,
    getNextPageParam: (page) => page.next_after_revision ?? undefined,
    enabled: projectId.length > 0 && sessionId.length > 0,
    staleTime: Number.POSITIVE_INFINITY,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
}

export function useProjectSessionRuns(
  projectId: string,
  sessionId: string,
  status?: RunStatus,
) {
  return useInfiniteQuery({
    queryKey: projectSessionKeys.runs(projectId, sessionId, status),
    queryFn: ({ pageParam, signal }) =>
      getJson<ProjectRunPage>(
        withSearchParams(
          `${projectSessionEndpoint(projectId, sessionId)}/runs`,
          {
            status,
            cursor: pageParam,
            limit: RUN_PAGE_SIZE,
          },
        ),
        signal,
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: projectId.length > 0 && sessionId.length > 0,
  })
}

export function useProjectHarnessRun(
  projectId: string,
  sessionId: string,
  runId: string,
) {
  return useQuery({
    queryKey: projectSessionKeys.run(projectId, sessionId, runId),
    queryFn: ({ signal }) =>
      fetchProjectHarnessRun(projectId, sessionId, runId, signal),
    enabled:
      projectId.length > 0 && sessionId.length > 0 && runId.length > 0,
  })
}

export function useProjectRunEvents(
  projectId: string,
  sessionId: string,
  runId: string,
  enabled = true,
) {
  return useInfiniteQuery({
    queryKey: projectSessionKeys.events(projectId, sessionId, runId),
    queryFn: ({ pageParam, signal }) =>
      fetchProjectRunEventsAfter(
        projectId,
        sessionId,
        runId,
        pageParam,
        signal,
      ),
    initialPageParam: 0,
    getNextPageParam: (page) => page.next_after_sequence ?? undefined,
    enabled: enabled &&
      projectId.length > 0 && sessionId.length > 0 && runId.length > 0,
  })
}

export function useStartProjectHarnessRun(
  projectId: string,
  sessionId: string,
) {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ idempotencyKey, request }: StartProjectHarnessRunVariables) =>
      postJson<StartProjectHarnessRunResponse>(
        `${projectSessionEndpoint(projectId, sessionId)}/runs`,
        request,
        { headers: { 'Idempotency-Key': idempotencyKey } },
      ),
    onSuccess: (response) => {
      const current = queryClient.getQueryData<ProjectHarnessSession>(
        projectSessionKeys.detail(projectId, sessionId),
      )
      seedAcceptedRun(queryClient, projectId, response, {
        session_id: sessionId,
        current_revision: response.trigger_message.revision,
        created_at: current?.session.created_at ?? response.run.created_at,
      }, current?.harness_id ?? '')
    },
  })
}

export function useAbortProjectHarnessRun(
  projectId: string,
  sessionId: string,
  runId: string,
) {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: (request: AbortProjectHarnessRunRequest) =>
      postJson<RunAbortResult>(
        `${projectRunEndpoint(projectId, sessionId, runId)}/abort`,
        request,
      ),
    onSuccess: ({ run }) => {
      setProjectRunData(queryClient, projectId, sessionId, run)
      void settleProjectRunQueries(queryClient, projectId, sessionId, runId)
    },
  })
}

export function fetchProjectHarnessRun(
  projectId: string,
  sessionId: string,
  runId: string,
  signal?: AbortSignal,
) {
  return getJson<ProjectRun>(
    projectRunEndpoint(projectId, sessionId, runId),
    signal,
  )
}

export function fetchProjectRunEventsAfter(
  projectId: string,
  sessionId: string,
  runId: string,
  afterSequence: number,
  signal?: AbortSignal,
) {
  return getJson<RunEventPage>(
    withSearchParams(`${projectRunEndpoint(projectId, sessionId, runId)}/events`, {
      after_sequence: afterSequence > 0 ? afterSequence : undefined,
      limit: EVENT_PAGE_SIZE,
    }),
    signal,
  )
}

export function projectRunEventStreamEndpoint(
  projectId: string,
  sessionId: string,
  runId: string,
  afterSequence: number,
) {
  return withSearchParams(
    `${projectRunEndpoint(projectId, sessionId, runId)}/events/stream`,
    { after_sequence: afterSequence > 0 ? afterSequence : undefined },
  )
}

export function appendProjectRunEvents(
  queryClient: QueryClient,
  projectId: string,
  sessionId: string,
  runId: string,
  incoming: readonly RunEvent[],
) {
  if (incoming.length === 0) {
    return
  }
  const queryKey = projectSessionKeys.events(projectId, sessionId, runId)
  const current = queryClient.getQueryData<InfiniteData<RunEventPage, number>>(
    queryKey,
  )
  const existingIds = new Set(
    current?.pages.flatMap((page) => page.items.map((event) => event.event_id)),
  )
  const additions = incoming
    .filter(
      (event) => event.run_id === runId && !existingIds.has(event.event_id),
    )
    .sort((left, right) => left.sequence - right.sequence)
  if (additions.length === 0) {
    return
  }
  queryClient.setQueryData<InfiniteData<RunEventPage, number>>(queryKey, () => {
    if (current === undefined) {
      return {
        pages: [{ items: additions, next_after_sequence: null }],
        pageParams: [0],
      }
    }
    const pages = [...current.pages]
    const lastIndex = pages.length - 1
    const lastPage = pages[lastIndex]
    pages[lastIndex] = {
      ...lastPage,
      items: [...lastPage.items, ...additions].sort(
        (left, right) => left.sequence - right.sequence,
      ),
      next_after_sequence: null,
    }
    return { ...current, pages }
  })
  for (const event of additions) {
    applyProjectRunEvent(queryClient, projectId, sessionId, runId, event)
  }
}

export function getLastProjectRunEventSequence(
  queryClient: QueryClient,
  projectId: string,
  sessionId: string,
  runId: string,
) {
  const data = queryClient.getQueryData<InfiniteData<RunEventPage, number>>(
    projectSessionKeys.events(projectId, sessionId, runId),
  )
  return data?.pages.reduce(
    (highest, page) =>
      page.items.reduce(
        (pageHighest, event) => Math.max(pageHighest, event.sequence),
        highest,
      ),
    0,
  ) ?? 0
}

export async function settleProjectRunQueries(
  queryClient: QueryClient,
  projectId: string,
  sessionId: string,
  runId: string,
) {
  await Promise.all([
    queryClient.invalidateQueries({
      queryKey: projectSessionKeys.detail(projectId, sessionId),
    }),
    queryClient.invalidateQueries({
      queryKey: projectSessionKeys.messages(projectId, sessionId),
    }),
    queryClient.invalidateQueries({
      queryKey: projectSessionKeys.runsRoot(projectId, sessionId),
    }),
    queryClient.invalidateQueries({
      queryKey: projectSessionKeys.run(projectId, sessionId, runId),
    }),
    queryClient.invalidateQueries({ queryKey: projectKeys.environments(projectId) }),
  ])
}

function seedAcceptedRun(
  queryClient: QueryClient,
  projectId: string,
  response:
    | CreateProjectHarnessSessionResponse
    | StartProjectHarnessRunResponse,
  session: CreateProjectHarnessSessionResponse['session'],
  harnessId: string,
) {
  const sessionId = session.session_id
  const existing = queryClient.getQueryData<ProjectHarnessSession>(
    projectSessionKeys.detail(projectId, sessionId),
  )
  const detail: ProjectHarnessSession = {
    session,
    harness_id: existing?.harness_id ?? harnessId,
    latest_run: response.run,
    active_run: isLiveRunStatus(response.run.status) ? response.run : null,
  }
  if (detail.harness_id.length > 0) {
    queryClient.setQueryData(projectSessionKeys.detail(projectId, sessionId), detail)
  }
  appendSessionMessage(queryClient, projectId, sessionId, response.trigger_message)
  setProjectRunData(queryClient, projectId, sessionId, response.run)
  appendSessionRun(queryClient, projectId, sessionId, response.run)
  void queryClient.invalidateQueries({
    queryKey: projectSessionKeys.detail(projectId, sessionId),
    refetchType: 'none',
  })
  void queryClient.invalidateQueries({
    queryKey: projectSessionKeys.messages(projectId, sessionId),
    refetchType: 'none',
  })
}

function appendSessionMessage(
  queryClient: QueryClient,
  projectId: string,
  sessionId: string,
  message: SessionMessage,
) {
  queryClient.setQueryData<InfiniteData<SessionMessagePage, number>>(
    projectSessionKeys.messages(projectId, sessionId),
    (current) => {
      if (
        current?.pages.some((page) =>
          page.items.some(
            (candidate) =>
              candidate.session_message_id === message.session_message_id,
          ),
        )
      ) {
        return current
      }
      if (current === undefined) {
        return {
          pages: [{ items: [message], next_after_revision: null }],
          pageParams: [0],
        }
      }
      const pages = [...current.pages]
      const lastIndex = pages.length - 1
      const lastPage = pages[lastIndex]
      pages[lastIndex] = {
        ...lastPage,
        items: [...lastPage.items, message].sort(
          (left, right) => left.revision - right.revision,
        ),
      }
      return { ...current, pages }
    },
  )
}

function appendSessionRun(
  queryClient: QueryClient,
  projectId: string,
  sessionId: string,
  run: ProjectRun,
) {
  const queryKey = projectSessionKeys.runs(projectId, sessionId)
  queryClient.setQueryData<InfiniteData<ProjectRunPage, string | undefined>>(
    queryKey,
    (current) => {
      const summary = runSummary(run)
      if (current === undefined) {
        return {
          pages: [{ items: [summary], next_cursor: null }],
          pageParams: [undefined],
        }
      }
      if (
        current.pages.some((page) =>
          page.items.some((candidate) => candidate.run_id === run.run_id),
        )
      ) {
        return current
      }
      const pages = [...current.pages]
      const lastIndex = pages.length - 1
      const lastPage = pages[lastIndex]
      pages[lastIndex] = { ...lastPage, items: [...lastPage.items, summary] }
      return { ...current, pages }
    },
  )
}

function setProjectRunData(
  queryClient: QueryClient,
  projectId: string,
  sessionId: string,
  run: ProjectRun,
) {
  queryClient.setQueryData(
    projectSessionKeys.run(projectId, sessionId, run.run_id),
    run,
  )
  queryClient.setQueryData<ProjectHarnessSession>(
    projectSessionKeys.detail(projectId, sessionId),
    (current) =>
      current === undefined
        ? current
        : {
            ...current,
            latest_run: run,
            active_run: isLiveRunStatus(run.status) ? run : null,
          },
  )
}

function applyProjectRunEvent(
  queryClient: QueryClient,
  projectId: string,
  sessionId: string,
  runId: string,
  event: RunEvent,
) {
  queryClient.setQueryData<ProjectRun>(
    projectSessionKeys.run(projectId, sessionId, runId),
    (current) => {
      if (current === undefined) return current
      const currentTurn = event.turn_number ?? current.current_turn
      if (
        current.status === event.run_status &&
        current.state_version === event.state_version &&
        current.current_turn === currentTurn
      ) {
        return current
      }
      return {
        ...current,
        status: event.run_status,
        state_version: event.state_version,
        current_turn: currentTurn,
      }
    },
  )
  queryClient.setQueryData<ProjectHarnessSession>(
    projectSessionKeys.detail(projectId, sessionId),
    (current) => {
      if (current === undefined) {
        return current
      }
      if (current.latest_run?.run_id !== runId) {
        return current
      }
      const currentTurn = event.turn_number ?? current.latest_run.current_turn
      if (
        current.latest_run.status === event.run_status &&
        current.latest_run.state_version === event.state_version &&
        current.latest_run.current_turn === currentTurn
      ) {
        return current
      }
      const candidate = {
        ...current.latest_run,
        status: event.run_status,
        state_version: event.state_version,
        current_turn: currentTurn,
      }
      return {
        ...current,
        latest_run: candidate,
        active_run: !isTerminalRunStatus(event.run_status) ? candidate : null,
      }
    },
  )
}

function runSummary(run: ProjectRun): ProjectRunSummary {
  const { resolved_config: _resolvedConfig, ...summary } = run
  return summary
}

function projectSessionsEndpoint(projectId: string) {
  return `/api/projects/${encodeURIComponent(projectId)}/harness-sessions`
}

function projectSessionEndpoint(projectId: string, sessionId: string) {
  return `${projectSessionsEndpoint(projectId)}/${encodeURIComponent(sessionId)}`
}

function projectRunEndpoint(
  projectId: string,
  sessionId: string,
  runId: string,
) {
  return `${projectSessionEndpoint(projectId, sessionId)}/runs/${encodeURIComponent(runId)}`
}

function withSearchParams(
  path: string,
  values: Record<string, string | number | undefined>,
) {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(values)) {
    if (value !== undefined) {
      search.set(key, String(value))
    }
  }
  const query = search.toString()
  return query.length > 0 ? `${path}?${query}` : path
}
