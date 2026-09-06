import { mutationOptions, queryOptions, useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import { ApiError, deleteRequest, getJson, patchJson, postJson } from '../../lib/api-client'
import type { ProjectBootstrap } from './project-bootstrap'
import type { CreateProjectInput, Project, ProjectEnvironment } from './project-types'

export const projectKeys = {
  all: ['projects'] as const,
  list: () => [...projectKeys.all, 'list'] as const,
  detail: (projectId: string) => [...projectKeys.all, 'detail', projectId] as const,
  bootstrap: (projectId: string) => [...projectKeys.detail(projectId), 'bootstrap'] as const,
  harnesses: (projectId: string) => [...projectKeys.detail(projectId), 'harnesses'] as const,
  environments: (projectId: string) => [...projectKeys.detail(projectId), 'environments'] as const,
}

export function projectBootstrapOptions(projectId: string) {
  return queryOptions({
    queryKey: projectKeys.bootstrap(projectId),
    queryFn: ({ signal }) => getJson<ProjectBootstrap>(`/api/projects/${encodeURIComponent(projectId)}/bootstrap`, signal),
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

export function useProjectBootstrap(projectId: string) {
  return useQuery(projectBootstrapOptions(projectId))
}

export function useProjectEnvironments(projectId: string) {
  return useQuery({
    queryKey: projectKeys.environments(projectId),
    queryFn: ({ signal }) => getJson<ProjectEnvironment[]>(
      `/api/projects/${encodeURIComponent(projectId)}/environments`, signal,
    ),
    staleTime: 30_000,
    gcTime: 10 * 60_000,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
    refetchInterval: 15_000,
    refetchIntervalInBackground: false,
  })
}

type EnvironmentAction = { environmentId: string; action: 'rename'; name: string } | { environmentId: string; action: 'delete' }

export function environmentActionOptions(client: QueryClient, projectId: string) {
  const environmentsKey = projectKeys.environments(projectId)
  const bootstrapKey = projectKeys.bootstrap(projectId)
  return mutationOptions({
    mutationKey: [...environmentsKey, 'actions'],
    retry: false,
    gcTime: 0,
    mutationFn: async (input: EnvironmentAction) => {
      const path = `/api/projects/${encodeURIComponent(projectId)}/environments/${encodeURIComponent(input.environmentId)}`
      if (input.action === 'rename') return patchJson<ProjectEnvironment>(path, { name: input.name })
      await deleteRequest(path)
      return null
    },
    onSuccess: async (environment, input) => {
      await Promise.all([client.cancelQueries({ queryKey: environmentsKey }), client.cancelQueries({ queryKey: bootstrapKey })])
      const update = (items: ProjectEnvironment[]) => items
        .flatMap(item => item.id !== input.environmentId ? [item] : environment ? [environment] : [])
        .sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }) || a.id.localeCompare(b.id))
      // Update existing caches only, keeping other environments and bootstrap data intact.
      client.setQueryData<ProjectEnvironment[]>(environmentsKey, previous => previous ? update(previous) : undefined)
      client.setQueryData<ProjectBootstrap>(bootstrapKey, previous => previous ? { ...previous, project_environments: update(previous.project_environments) } : undefined)
    },
    onSettled: () => {
      // Reconcile uncertain failures too, without retrying a destructive request.
      void client.invalidateQueries({ queryKey: environmentsKey })
      void client.invalidateQueries({ queryKey: bootstrapKey })
    },
  })
}

export function useEnvironmentAction(projectId: string) {
  return useMutation(environmentActionOptions(useQueryClient(), projectId))
}

export const projectsOptions = queryOptions({
  queryKey: projectKeys.list(),
  queryFn: ({ signal }) => getJson<Project[]>('/api/projects', signal),
  staleTime: 30_000,
  gcTime: 10 * 60_000,
  refetchOnWindowFocus: true,
  refetchOnReconnect: true,
  refetchInterval: 15_000,
  refetchIntervalInBackground: false,
})

export function useProjects() {
  return useQuery(projectsOptions)
}

export function useCreateProject() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (input: CreateProjectInput) => postJson<Project>('/api/projects', input),
    retry: false,
    gcTime: 0,
    onSuccess: async (project) => {
      await client.cancelQueries({ queryKey: projectKeys.list() })
      client.setQueryData<Project[]>(projectKeys.list(), (previous = []) =>
        [...previous.filter((existing) => existing.id !== project.id), project]
          .sort((left, right) => left.name.localeCompare(right.name, undefined, { sensitivity: 'base' }) || left.id.localeCompare(right.id)),
      )
      client.setQueryData(projectKeys.detail(project.id), project)
      void client.invalidateQueries({ queryKey: projectKeys.list() })
    },
    onSettled: (_data, _error, input) => {
      // Avoid retaining a potentially large data URL in the mutation cache.
      input.avatar = null
    },
  })
}
