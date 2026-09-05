import { infiniteQueryOptions, useInfiniteQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { ApiError, getJson, patchJson } from '../../lib/api-client'

export type ProjectSession = {
  id: string
  project_id: string
  title: string | null
  active_run: { id: string; status: 'ready' | 'running' | 'waiting' } | null
}
type SessionPage = { items: ProjectSession[]; next_cursor: string | null }
export const sessionKeys = { list: (projectId: string) => ['projects', 'detail', projectId, 'sessions'] as const }
export function sessionsOptions(projectId: string) {
  return infiniteQueryOptions({
    queryKey: sessionKeys.list(projectId),
    initialPageParam: null as string | null,
    queryFn: ({ pageParam, signal }) => getJson<SessionPage>(`/api/projects/${encodeURIComponent(projectId)}/sessions?limit=50${pageParam ? `&cursor=${encodeURIComponent(pageParam)}` : ''}`, signal),
    getNextPageParam: (page, _pages, _param, params) => page.next_cursor && !params.includes(page.next_cursor) ? page.next_cursor : undefined,
    enabled: /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(projectId),
    staleTime: 5_000,
    gcTime: 10 * 60_000,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
    refetchInterval: query => query.state.data?.pages.some(p => p.items.some(s => s.active_run !== null)) ? 3_000 : 30_000,
    refetchIntervalInBackground: false,
    retry: (count, error) => count < 2 && (!(error instanceof ApiError) || error.status === 408 || error.status === 429 || error.status >= 500),
    retryDelay: attempt => Math.min(1000 * 2 ** attempt, 10_000),
  })
}
export function useProjectSessions(projectId: string) { return useInfiniteQuery(sessionsOptions(projectId)) }
export function useArchiveSession(projectId: string) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => patchJson(`/api/sessions/${encodeURIComponent(id)}`, { archived: true }),
    retry: false,
    onSuccess: () => client.invalidateQueries({ queryKey: sessionKeys.list(projectId) }),
  })
}
