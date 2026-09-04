import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getJson, postJson } from '../../lib/api-client'
import type { CreateProjectInput, Project, ProjectEnvironment } from './project-types'

export const projectKeys = {
  all: ['projects'] as const,
  list: () => [...projectKeys.all, 'list'] as const,
  detail: (projectId: string) => [...projectKeys.all, 'detail', projectId] as const,
  environments: (projectId: string) => [...projectKeys.detail(projectId), 'environments'] as const,
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
