import { useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { refreshChat } from './chat-queries'
import { projectKeys } from './project-queries'
import type { RunEvent } from './project-session-types'

// Match the lifecycle event names emitted by the platform runtime.
export const RUN_EVENT_TYPES = [
  'run.created', 'run.started', 'run.resumed', 'run.committed',
  'run.input_received', 'run.abort_requested', 'run.yielded', 'run.waiting',
  'run.woken', 'run.wait_resolved', 'run.completed', 'run.failed',
  'run.aborted', 'run.lease_expired',
] as const
export function useChatStream(project: string, session: string, runId: string | undefined) {
  const client = useQueryClient()
  const [reconnecting, setReconnecting] = useState(false)
  useEffect(() => {
    if (!runId) return
    let disposed = false
    let finished = false
    let source: EventSource | undefined
    let cursor = 0
    let attempt = 0
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined
    let refreshTimer: ReturnType<typeof setTimeout> | undefined
    const refresh = () => { void refreshChat(client, project, session) }
    const scheduleRefresh = () => {
      if (refreshTimer) return
      refreshTimer = setTimeout(() => { refreshTimer = undefined; if (!disposed) refresh() }, 150)
    }
    const reconnect = () => {
      source?.close()
      if (disposed || finished || reconnectTimer) return
      setReconnecting(true)
      scheduleRefresh()
      reconnectTimer = setTimeout(() => { reconnectTimer = undefined; connect() }, Math.min(500 * 2 ** attempt++, 10_000))
    }
    const receive = (message: MessageEvent<string>) => {
      try {
        const event: RunEvent = JSON.parse(message.data)
        if (event.run_id !== runId || !Number.isSafeInteger(event.sequence) || typeof event.id !== 'string') throw new Error('Invalid event')
        if (event.sequence <= cursor) return
        cursor = event.sequence
        scheduleRefresh()
        if (['run.completed', 'run.failed', 'run.aborted'].includes(event.type)) {
          finished = true
          clearTimeout(reconnectTimer)
          source?.close()
          setReconnecting(false)
          void client.invalidateQueries({ queryKey: projectKeys.bootstrap(project) })
          void client.invalidateQueries({ queryKey: projectKeys.environments(project) })
        }
      } catch { reconnect() }
    }
    function connect() {
      if (disposed || finished) return
      source = new EventSource(`/api/runs/${runId}/events/stream?after_sequence=${cursor}`)
      source.onopen = () => { attempt = 0; setReconnecting(false); scheduleRefresh() }
      for (const name of RUN_EVENT_TYPES) source.addEventListener(name, receive)
      source.onerror = reconnect
    }
    connect()
    return () => { disposed = true; source?.close(); clearTimeout(reconnectTimer); clearTimeout(refreshTimer) }
  }, [client, project, session, runId])
  return Boolean(runId) && reconnecting
}
