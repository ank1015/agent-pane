import { queryOptions, useQuery, type InfiniteData, type QueryClient } from '@tanstack/react-query'
import { ApiError, getJson } from '../../lib/api-client'
import { sessionKeys, type ProjectSession } from './session-queries'
import type { AcceptedRun, ChatSession, ProjectRunSummary, RunInput, SessionMessage } from './project-session-types'
import { isLiveRunStatus } from './project-session-types'

export const validId = (id: string) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)
export const chatKeys = {
  root: (id: string) => ['chat', id] as const,
  session: (id: string) => ['chat', id, 'session'] as const,
  messages: (id: string) => ['chat', id, 'messages'] as const,
  runs: (id: string) => ['chat', id, 'runs'] as const,
  inputs: (id: string) => ['chat', id, 'inputs'] as const,
}
export const retryRead = (count: number, error: Error) => count < 2 && (!(error instanceof ApiError) || [408, 429].includes(error.status) || error.status >= 500)
const common = { staleTime: 5_000, gcTime: 10 * 60_000, retry: retryRead, retryDelay: (attempt: number) => Math.min(1_000 * 2 ** attempt, 10_000), refetchOnWindowFocus: true, refetchOnReconnect: true, refetchIntervalInBackground: false }

export async function readCursorPages<T>(path: string, signal: AbortSignal): Promise<T[]> {
  const items: T[] = []
  const seen = new Set<string>()
  let cursor: string | null = null
  do {
    const page: { items: T[]; next_cursor: string | null } = await getJson(`${path}?limit=200${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`, AbortSignal.any([signal, AbortSignal.timeout(30_000)]))
    items.push(...page.items)
    cursor = page.next_cursor
    if (cursor && seen.has(cursor)) throw new Error('The server repeated a pagination cursor.')
    if (cursor) seen.add(cursor)
  } while (cursor)
  return items
}
export function chatSessionOptions(id: string) {
  return queryOptions({ ...common, queryKey: chatKeys.session(id), enabled: validId(id),
    queryFn: ({ signal }) => getJson<ChatSession>(`/api/sessions/${id}`, AbortSignal.any([signal, AbortSignal.timeout(30_000)])),
    refetchInterval: query => query.state.data?.active_run ? 3_000 : 30_000,
  })
}
export function useChatSession(id: string) { return useQuery(chatSessionOptions(id)) }
export function chatRunsOptions(id: string, live: boolean) {
  return queryOptions({ ...common, queryKey: chatKeys.runs(id), enabled: validId(id),
    queryFn: ({ signal }) => readCursorPages<ProjectRunSummary>(`/api/sessions/${id}/runs`, signal),
    refetchInterval: live ? 5_000 : 30_000,
  })
}
export function chatInputsOptions(id: string, live: boolean) {
  return queryOptions({ ...common, queryKey: chatKeys.inputs(id), enabled: validId(id),
    queryFn: ({ signal }) => readCursorPages<RunInput>(`/api/sessions/${id}/inputs`, signal),
    refetchInterval: live ? 5_000 : 30_000,
  })
}
export function chatMessagesOptions(client: QueryClient, id: string, live: boolean) {
  return queryOptions({ ...common, queryKey: chatKeys.messages(id), enabled: validId(id),
    refetchInterval: live ? 5_000 : 30_000,
    queryFn: async ({ signal }) => {
      const previous = client.getQueryData<SessionMessage[]>(chatKeys.messages(id)) ?? []
      const messages = [...previous]
      let after = previous.reduce((max, m) => Math.max(max, m.revision), 0)
      while (true) {
        const page = await getJson<{ items: SessionMessage[]; next_after_revision: number | null }>(`/api/sessions/${id}/messages?limit=200&after_revision=${after}`, AbortSignal.any([signal, AbortSignal.timeout(30_000)]))
        messages.push(...page.items)
        if (page.next_after_revision === null) break
        if (page.next_after_revision <= after) throw new Error('The server repeated a message cursor.')
        after = page.next_after_revision
      }
      return [...new Map(messages.map(m => [m.message_id, m])).values()].sort((a, b) => a.revision - b.revision)
    },
  })
}
export function refreshChat(client: QueryClient, project: string, session: string) {
  return Promise.all([
    client.invalidateQueries({ queryKey: chatKeys.root(session) }, { cancelRefetch: false }),
    client.invalidateQueries({ queryKey: sessionKeys.list(project) }, { cancelRefetch: false }),
  ])
}
export function seedAccepted(client: QueryClient, project: string, sessionId: string, reply: AcceptedRun) {
  client.setQueryData<ChatSession>(chatKeys.session(sessionId), old => old ?? reply.session)
  const cachedRun = client.getQueryData<ProjectRunSummary[]>(chatKeys.runs(sessionId))?.find(r => r.id === reply.run.id)
  const acceptedRun = cachedRun && cachedRun.version > reply.run.version ? cachedRun : reply.run
  client.setQueryData<ChatSession>(chatKeys.session(sessionId), old => {
    if (!old || (old.active_run && old.active_run.id !== acceptedRun.id && old.active_run.created_at > acceptedRun.created_at)) return old
    return { ...old, active_run: isLiveRunStatus(acceptedRun.status) ? acceptedRun : null }
  })
  client.setQueryData<ProjectRunSummary[]>(chatKeys.runs(sessionId), old => old || reply.session ? [...(old ?? []).filter(r => r.id !== acceptedRun.id), acceptedRun].sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id)) : old)
  client.setQueryData<RunInput[]>(chatKeys.inputs(sessionId), old => {
    if (!old && !reply.session) return old
    // A replayed receipt may predate input handling.
    if (old?.some(i => i.id === reply.input.id)) return old
    return [...(old ?? []), reply.input]
  })
  // A new session is known to have no history. Never seed a partial history for an existing session.
  if (reply.session) client.setQueryData<SessionMessage[]>(chatKeys.messages(sessionId), old => old ?? [])
  client.setQueryData<InfiniteData<{items: ProjectSession[]; next_cursor: string | null}>>(sessionKeys.list(project), old => {
    if (!old || !reply.session) return old
    const pages = old.pages.map(p => ({ ...p, items: p.items.filter(s => s.id !== sessionId) }))
    const active = reply.session.active_run
    const summary: ProjectSession = { id: reply.session.id, project_id: reply.session.project_id, title: reply.session.title, active_run: active && (active.status === 'ready' || active.status === 'running' || active.status === 'waiting') ? { id: active.id, status: active.status } : null }
    pages[0] = { ...pages[0], items: [summary, ...pages[0].items] }
    return { ...old, pages }
  })
  void refreshChat(client, project, sessionId)
}
