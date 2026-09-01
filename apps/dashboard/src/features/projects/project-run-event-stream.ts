import { useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import {
  appendProjectRunEvents,
  fetchProjectHarnessRun,
  fetchProjectRunEventsAfter,
  getLastProjectRunEventSequence,
  projectRunEventStreamEndpoint,
  projectSessionKeys,
  settleProjectRunQueries,
} from './project-session-queries'
import {
  type RunEvent,
  type RunStatus,
  isLiveRunStatus,
  isTerminalRunStatus,
} from './project-session-types'

const EVENT_NAMES = [
  'run.started',
  'turn.requested',
  'turn.started',
  'turn.ended',
  'run.waiting',
  'run.resumed',
  'run.completed',
  'run.failed',
  'run.aborted',
  'progress',
] as const
const INITIAL_RETRY_DELAY_MS = 500
const MAX_RETRY_DELAY_MS = 5_000

export type RunEventStreamState =
  | 'idle'
  | 'connecting'
  | 'connected'
  | 'reconnecting'
  | 'closed'
  | 'error'

type ProjectRunEventStreamOptions = {
  projectId: string
  sessionId: string
  runId: string
  runStatus?: RunStatus
  enabled?: boolean
}

export function useProjectRunEventStream({
  projectId,
  sessionId,
  runId,
  runStatus,
  enabled = true,
}: ProjectRunEventStreamOptions) {
  const queryClient = useQueryClient()
  const [state, setState] = useState<RunEventStreamState>('idle')
  const [error, setError] = useState<Error | null>(null)
  const canConnect =
    enabled &&
    projectId.length > 0 &&
    sessionId.length > 0 &&
    runId.length > 0 &&
    (runStatus === undefined || isLiveRunStatus(runStatus))

  useEffect(() => {
    if (!canConnect) {
      return
    }

    let disposed = false
    let source: EventSource | null = null
    let retryTimer: number | undefined
    let retryAttempt = 0
    const recoveryController = new AbortController()

    const closeSource = () => {
      source?.close()
      source = null
    }

    const finish = () => {
      closeSource()
      if (!disposed) {
        setState('closed')
        setError(null)
      }
      void settleProjectRunQueries(queryClient, projectId, sessionId, runId)
    }

    const scheduleReconnect = () => {
      if (disposed) {
        return
      }
      const delay = Math.min(
        INITIAL_RETRY_DELAY_MS * 2 ** retryAttempt,
        MAX_RETRY_DELAY_MS,
      )
      retryAttempt += 1
      retryTimer = window.setTimeout(connect, delay)
    }

    const recover = async () => {
      try {
        const afterSequence = getLastProjectRunEventSequence(
          queryClient,
          projectId,
          sessionId,
          runId,
        )
        const page = await fetchProjectRunEventsAfter(
          projectId,
          sessionId,
          runId,
          afterSequence,
          recoveryController.signal,
        )
        if (disposed) {
          return
        }
        appendProjectRunEvents(
          queryClient,
          projectId,
          sessionId,
          runId,
          page.items,
        )
        if (page.items.some((event) => isTerminalRunStatus(event.run_status))) {
          finish()
          return
        }
        const run = await queryClient.fetchQuery({
          queryKey: projectSessionKeys.run(projectId, sessionId, runId),
          queryFn: ({ signal }) =>
            fetchProjectHarnessRun(projectId, sessionId, runId, signal),
          staleTime: 0,
        })
        if (isTerminalRunStatus(run.status)) {
          finish()
          return
        }
        scheduleReconnect()
      } catch (cause) {
        if (disposed || recoveryController.signal.aborted) {
          return
        }
        setState('error')
        setError(asError(cause))
        scheduleReconnect()
      }
    }

    function connect() {
      if (disposed) {
        return
      }
      closeSource()
      const afterSequence = getLastProjectRunEventSequence(
        queryClient,
        projectId,
        sessionId,
        runId,
      )
      setState(retryAttempt === 0 ? 'connecting' : 'reconnecting')
      setError(null)
      source = new EventSource(
        projectRunEventStreamEndpoint(
          projectId,
          sessionId,
          runId,
          afterSequence,
        ),
      )
      source.onopen = () => {
        if (!disposed) {
          retryAttempt = 0
          setState('connected')
          setError(null)
        }
      }
      for (const eventName of EVENT_NAMES) {
        source.addEventListener(eventName, (message) => {
          if (disposed) {
            return
          }
          try {
            const event = parseRunEvent((message as MessageEvent<string>).data, runId)
            appendProjectRunEvents(
              queryClient,
              projectId,
              sessionId,
              runId,
              [event],
            )
            if (isTerminalRunStatus(event.run_status)) {
              finish()
            }
          } catch (cause) {
            closeSource()
            setState('error')
            setError(asError(cause))
            void recover()
          }
        })
      }
      source.onerror = () => {
        closeSource()
        if (!disposed) {
          setState('reconnecting')
          void recover()
        }
      }
    }

    connect()

    return () => {
      disposed = true
      closeSource()
      recoveryController.abort()
      if (retryTimer !== undefined) {
        window.clearTimeout(retryTimer)
      }
    }
  }, [canConnect, projectId, queryClient, runId, sessionId])

  return {
    state: !enabled || runId.length === 0
      ? 'idle'
      : runStatus !== undefined && isTerminalRunStatus(runStatus)
        ? 'closed'
        : state,
    error: canConnect ? error : null,
  }
}

function parseRunEvent(data: string, expectedRunId: string): RunEvent {
  const parsed: unknown = JSON.parse(data)
  if (
    typeof parsed !== 'object' ||
    parsed === null ||
    !('event_id' in parsed) ||
    typeof parsed.event_id !== 'string' ||
    !('run_id' in parsed) ||
    parsed.run_id !== expectedRunId ||
    !('sequence' in parsed) ||
    typeof parsed.sequence !== 'number' ||
    !('run_status' in parsed) ||
    typeof parsed.run_status !== 'string' ||
    !('type' in parsed) ||
    typeof parsed.type !== 'string'
  ) {
    throw new Error('Platform returned an invalid run event.')
  }
  return parsed as RunEvent
}

function asError(cause: unknown) {
  return cause instanceof Error ? cause : new Error('Run event stream failed.')
}
