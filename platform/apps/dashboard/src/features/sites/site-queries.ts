import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { deleteRequest, getJson, patchJson } from '../../lib/api-client'
export type Site = { id: string; name: string; desired_status: string; updated_at: string }
export type SiteDetail = { site: Site; resource: { status: string; active_release_id: string | null; error?: unknown } }
export const siteKeys = {
  list: (project: string) => ['project-sites', project] as const,
  detail: (project: string, site: string) => ['site-detail', project, site] as const,
  snapshots: (project: string, site: string) => ['site-snapshots', project, site] as const,
}
export const sitePath = (project: string, site?: string) => `/api/site-view/projects/${encodeURIComponent(project)}/sites${site ? '/' + encodeURIComponent(site) : ''}`
export function useSites(project: string, enabled = true) {
  return useQuery({ queryKey: siteKeys.list(project), queryFn: ({ signal }) => getJson<{ items: Site[] }>(sitePath(project), signal), enabled, retry: false, refetchInterval: 5_000 })
}
export function useSite(project: string, site: string) {
  return useQuery({ queryKey: siteKeys.detail(project, site), queryFn: ({ signal }) => getJson<SiteDetail>(sitePath(project, site), signal), retry: false, refetchInterval: 5_000 })
}

export function useSiteAction(project: string) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (input: { siteId: string; action: 'rename'; name: string } | { siteId: string; action: 'delete' }) => input.action === 'rename'
      ? patchJson(sitePath(project, input.siteId), { name: input.name })
      : deleteRequest(sitePath(project, input.siteId)),
    retry: false,
    onSuccess: async (_result, input) => {
      await client.cancelQueries({ queryKey: siteKeys.list(project) })
      client.setQueryData<{ items: Site[] }>(siteKeys.list(project), previous => previous && ({ ...previous, items: input.action === 'delete'
        ? previous.items.filter(site => site.id !== input.siteId)
        : previous.items.map(site => site.id === input.siteId ? { ...site, name: input.name } : site) }))
      await Promise.all([
        client.invalidateQueries({ queryKey: siteKeys.list(project) }),
        client.invalidateQueries({ queryKey: siteKeys.detail(project, input.siteId) }),
      ])
    },
  })
}
