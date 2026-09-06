import { mutationOptions, queryOptions, useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import { ApiError, getJson, putJson } from '../../lib/api-client'
import type { Harness, ProjectBootstrap } from './project-bootstrap'
import { projectKeys } from './project-queries'

export type ProjectHarness = Harness & {
  description: string | null
  project_policy: 'required' | 'opt_in'
  globally_enabled: boolean
  project_enabled: boolean
  available: boolean
}
export type HarnessCatalog = { items: ProjectHarness[] }
export type HarnessUpdate = { ids: string[]; enabled: boolean }
export type HarnessUpdateResult = { updated: ProjectHarness[]; failed: { id: string; message: string }[] }

export function projectHarnessesOptions(projectId: string) {
  return queryOptions({
    queryKey: projectKeys.harnesses(projectId),
    queryFn: ({ signal }) => getJson<HarnessCatalog>(`/api/projects/${encodeURIComponent(projectId)}/harnesses`, signal),
    enabled: /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(projectId),
    staleTime: 30_000,
    gcTime: 10 * 60_000,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
    refetchInterval: 30_000,
    refetchIntervalInBackground: false,
    retry: (count, error) => count < 2 && (!(error instanceof ApiError) || error.status === 408 || error.status === 429 || error.status >= 500),
    retryDelay: attempt => Math.min(1000 * 2 ** attempt, 10_000),
  })
}

export function harnessUpdateOptions(client: QueryClient, projectId: string) {
  const catalogKey = projectKeys.harnesses(projectId)
  const bootstrapKey = projectKeys.bootstrap(projectId)
  return mutationOptions({
    mutationKey: [...catalogKey, 'update'],
    retry: false,
    gcTime: 0,
    mutationFn: async ({ ids, enabled }: HarnessUpdate): Promise<HarnessUpdateResult> => {
      const unique = [...new Set(ids)]
      const results = await Promise.allSettled(unique.map(id => putJson<ProjectHarness>(
        `/api/projects/${encodeURIComponent(projectId)}/harnesses/${encodeURIComponent(id)}`, { enabled },
      )))
      return {
        updated: results.flatMap(result => result.status === 'fulfilled' ? [result.value] : []),
        failed: results.flatMap((result, index) => result.status === 'rejected' ? [{
          id: unique[index], message: result.reason instanceof Error ? result.reason.message : 'Could not update this harness.',
        }] : []),
      }
    },
    onSuccess: async ({ updated }) => {
      await Promise.all([client.cancelQueries({ queryKey: catalogKey }), client.cancelQueries({ queryKey: bootstrapKey })])
      const changes = new Map(updated.map(h => [h.id, h]))
      client.setQueryData<HarnessCatalog>(catalogKey, previous => previous ? {
        items: previous.items.map(h => changes.get(h.id) ?? h),
      } : undefined)
      client.setQueryData<ProjectBootstrap>(bootstrapKey, previous => previous ? {
        ...previous,
        harnesses: [...previous.harnesses.filter(h => !changes.has(h.id)), ...updated.filter(h => h.available)]
          .sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id)),
      } : undefined)
    },
    onSettled: () => {
      // Refresh uncertain outcomes as well; never discard the last good inventory.
      void client.invalidateQueries({ queryKey: catalogKey })
      void client.invalidateQueries({ queryKey: bootstrapKey })
    },
  })
}

export function useProjectHarnesses(projectId: string) {
  return useQuery(projectHarnessesOptions(projectId))
}
export function useHarnessUpdate(projectId: string) {
  return useMutation(harnessUpdateOptions(useQueryClient(), projectId))
}
