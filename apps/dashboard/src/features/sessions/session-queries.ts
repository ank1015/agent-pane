import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { getJson } from '../../lib/api-client'
import type {
  ProjectRunPage,
  ProjectSessionDetail,
  SessionMessagePage,
} from '../projects/project-session-types'

const MESSAGE_PAGE_SIZE = 100
const RUN_PAGE_SIZE = 100

export type SessionListItem = {
  id: string
  project_id: string
  project_name: string
  title: string
  harness_id: string
  is_active: boolean
  current_revision: number
  creation_state: string
  created_at: string
  last_activity_at: string
}

export const sessionKeys = {
  list: () => ['sessions', 'list'] as const,
  detail: (sessionId: string) => ['sessions', sessionId] as const,
  messages: (sessionId: string) =>
    [...sessionKeys.detail(sessionId), 'messages'] as const,
  runs: (sessionId: string) =>
    [...sessionKeys.detail(sessionId), 'runs'] as const,
}

export function useSessions() {
  return useQuery({
    queryKey: sessionKeys.list(),
    queryFn: ({ signal }) => getJson<SessionListItem[]>('/api/sessions', signal),
    staleTime: 5_000,
    refetchInterval: (query) =>
      query.state.data?.some((session) => session.is_active) === true
        ? 3_000
        : 30_000,
  })
}

export function useSession(sessionId: string) {
  return useQuery({
    queryKey: sessionKeys.detail(sessionId),
    queryFn: ({ signal }) =>
      getJson<ProjectSessionDetail>(sessionEndpoint(sessionId), signal),
    enabled: sessionId.length > 0,
  })
}

export function useSessionMessages(sessionId: string) {
  return useInfiniteQuery({
    queryKey: sessionKeys.messages(sessionId),
    queryFn: ({ pageParam, signal }) =>
      getJson<SessionMessagePage>(
        withSearchParams(`${sessionEndpoint(sessionId)}/messages`, {
          after_revision: pageParam > 0 ? pageParam : undefined,
          limit: MESSAGE_PAGE_SIZE,
        }),
        signal,
      ),
    initialPageParam: 0,
    getNextPageParam: (page) => page.next_after_revision ?? undefined,
    enabled: sessionId.length > 0,
    staleTime: Number.POSITIVE_INFINITY,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
}

export function useSessionRuns(sessionId: string) {
  return useInfiniteQuery({
    queryKey: sessionKeys.runs(sessionId),
    queryFn: ({ pageParam, signal }) =>
      getJson<ProjectRunPage>(
        withSearchParams(`${sessionEndpoint(sessionId)}/runs`, {
          cursor: pageParam,
          limit: RUN_PAGE_SIZE,
        }),
        signal,
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: sessionId.length > 0,
  })
}

function sessionEndpoint(sessionId: string) {
  return `/api/sessions/${encodeURIComponent(sessionId)}`
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
