import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  deleteRequest,
  getJson,
  patchJson,
  postJson,
} from '../../lib/api-client'

export type Project = {
  id: string
  name: string
  avatar: string | null
}

export type ProjectEnvironment = {
  id: string
  name: string
  host_name: string
  machine_id?: string
  path: string
  type: 'env' | 'template'
  snapshot_id?: string
  setup_script?: string
  created_at: number
}

export type CreateProjectInput = {
  name: string
  avatar: string | null
}

export const projectKeys = {
  all: ['projects'] as const,
  list: () => [...projectKeys.all, 'list'] as const,
  detail: (projectId: string) =>
    [...projectKeys.all, 'detail', projectId] as const,
  environments: (projectId: string) =>
    [...projectKeys.all, 'detail', projectId, 'environments'] as const,
}

export function useProjects() {
  return useQuery({
    queryKey: projectKeys.list(),
    queryFn: ({ signal }) => getJson<Project[]>('/api/projects', signal),
  })
}

export function useProject(projectId: string) {
  const queryClient = useQueryClient()

  return useQuery({
    queryKey: projectKeys.detail(projectId),
    queryFn: ({ signal }) =>
      getJson<Project>(
        `/api/projects/${encodeURIComponent(projectId)}`,
        signal,
      ),
    enabled: projectId.length > 0,
    initialData: () =>
      queryClient
        .getQueryData<Project[]>(projectKeys.list())
        ?.find((project) => project.id === projectId),
  })
}

export function useProjectEnvironments(projectId: string) {
  return useQuery({
    queryKey: projectKeys.environments(projectId),
    queryFn: ({ signal }) =>
      getJson<ProjectEnvironment[]>(
        `/api/projects/${encodeURIComponent(projectId)}/environments`,
        signal,
      ),
    enabled: projectId.length > 0,
  })
}

export function useUpdateProjectEnvironmentName(projectId: string) {
  const queryClient = useQueryClient()
  const queryKey = projectKeys.environments(projectId)

  return useMutation({
    mutationFn: ({
      environment,
      name,
    }: {
      environment: ProjectEnvironment
      name: string
    }) => patchJson<unknown>(projectEnvironmentEndpoint(environment), { name }),
    onMutate: async ({ environment, name }) => {
      await queryClient.cancelQueries({ queryKey })
      const previousEnvironments =
        queryClient.getQueryData<ProjectEnvironment[]>(queryKey)
      queryClient.setQueryData<ProjectEnvironment[]>(queryKey, (environments) =>
        environments?.map((candidate) =>
          candidate.id === environment.id ? { ...candidate, name } : candidate,
        ),
      )
      return { previousEnvironments }
    },
    onError: (_error, _variables, context) => {
      queryClient.setQueryData(queryKey, context?.previousEnvironments)
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey }),
  })
}

export function useDeleteProjectEnvironment(projectId: string) {
  const queryClient = useQueryClient()
  const queryKey = projectKeys.environments(projectId)

  return useMutation({
    mutationFn: (environment: ProjectEnvironment) =>
      deleteRequest(projectEnvironmentEndpoint(environment)),
    onMutate: async (environment) => {
      await queryClient.cancelQueries({ queryKey })
      const previousEnvironments =
        queryClient.getQueryData<ProjectEnvironment[]>(queryKey)
      queryClient.setQueryData<ProjectEnvironment[]>(queryKey, (environments) =>
        environments?.filter((candidate) => candidate.id !== environment.id),
      )
      return { previousEnvironments }
    },
    onError: (_error, _environment, context) => {
      queryClient.setQueryData(queryKey, context?.previousEnvironments)
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey }),
  })
}

export function useCreateProject() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: (project: CreateProjectInput) =>
      postJson<Project>('/api/projects', project),
    onSuccess: (project) => {
      queryClient.setQueryData(projectKeys.detail(project.id), project)
      return queryClient.invalidateQueries({ queryKey: projectKeys.list() })
    },
  })
}

function projectEnvironmentEndpoint(environment: ProjectEnvironment) {
  const resource =
    environment.type === 'template'
      ? 'sandbox-environment-templates'
      : 'environments'
  return `/api/machines/${resource}/${encodeURIComponent(environment.id)}`
}
