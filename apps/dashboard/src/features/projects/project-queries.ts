import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  deleteRequest,
  getJson,
  patchJson,
  postJson,
} from '../../lib/api-client'
import {
  harnessKeys,
  type HarnessModelOptions,
} from '../harnesses/harness-queries'
import type { ProviderKind } from '../providers/provider-queries'

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

export type ProjectBootstrapHarnessProvider = {
  provider_id: string
  model_ids: string[]
}

export type ProjectBootstrapHarness = {
  harness_id: string
  active_revision_id: string
  config_schema: Record<string, unknown> | null
  supported_providers: ProjectBootstrapHarnessProvider[]
  supported_reasoning_levels: string[]
}

export type ProjectBootstrapProviderAccount = {
  account_id: string
  name: string
  provider: ProviderKind
}

export type ProjectBootstrap = {
  harnesses: ProjectBootstrapHarness[]
  provider_accounts: ProjectBootstrapProviderAccount[]
  project_environments: ProjectEnvironment[]
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
  bootstrap: (projectId: string) =>
    [...projectKeys.all, 'detail', projectId, 'bootstrap'] as const,
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

export function useProjectBootstrap(projectId: string) {
  const queryClient = useQueryClient()

  return useQuery({
    queryKey: projectKeys.bootstrap(projectId),
    queryFn: async ({ signal }) => {
      const bootstrap = await getJson<ProjectBootstrap>(
        `/api/projects/${encodeURIComponent(projectId)}/bootstrap`,
        signal,
      )
      queryClient.setQueryData(
        projectKeys.environments(projectId),
        bootstrap.project_environments,
      )
      for (const harness of bootstrap.harnesses) {
        queryClient.setQueryData(
          harnessKeys.modelOptions(harness.harness_id),
          harnessModelOptions(bootstrap, harness.harness_id),
        )
      }
      return bootstrap
    },
    enabled: projectId.length > 0,
  })
}

export function harnessModelOptions(
  bootstrap: ProjectBootstrap,
  harnessId: string,
): HarnessModelOptions {
  const harness = bootstrap.harnesses.find(
    (candidate) => candidate.harness_id === harnessId,
  )
  if (harness === undefined) {
    return { providers: [], reasoning_levels: [] }
  }

  return {
    providers: harness.supported_providers.flatMap((supportedProvider) =>
      bootstrap.provider_accounts
        .filter(
          (account) => account.provider === supportedProvider.provider_id,
        )
        .map((account) => ({
          ...account,
          model_ids: supportedProvider.model_ids,
        })),
    ),
    reasoning_levels: harness.supported_reasoning_levels,
  }
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
      await Promise.all([
        queryClient.cancelQueries({ queryKey }),
        queryClient.cancelQueries({
          queryKey: projectKeys.bootstrap(projectId),
        }),
      ])
      const previousEnvironments =
        queryClient.getQueryData<ProjectEnvironment[]>(queryKey)
      const previousBootstrap = queryClient.getQueryData<ProjectBootstrap>(
        projectKeys.bootstrap(projectId),
      )
      updateProjectEnvironmentCaches(queryClient, projectId, (environments) =>
        environments.map((candidate) =>
          candidate.id === environment.id ? { ...candidate, name } : candidate,
        ),
      )
      return { previousBootstrap, previousEnvironments }
    },
    onError: (_error, _variables, context) => {
      queryClient.setQueryData(queryKey, context?.previousEnvironments)
      queryClient.setQueryData(
        projectKeys.bootstrap(projectId),
        context?.previousBootstrap,
      )
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
      await Promise.all([
        queryClient.cancelQueries({ queryKey }),
        queryClient.cancelQueries({
          queryKey: projectKeys.bootstrap(projectId),
        }),
      ])
      const previousEnvironments =
        queryClient.getQueryData<ProjectEnvironment[]>(queryKey)
      const previousBootstrap = queryClient.getQueryData<ProjectBootstrap>(
        projectKeys.bootstrap(projectId),
      )
      updateProjectEnvironmentCaches(queryClient, projectId, (environments) =>
        environments.filter((candidate) => candidate.id !== environment.id),
      )
      return { previousBootstrap, previousEnvironments }
    },
    onError: (_error, _environment, context) => {
      queryClient.setQueryData(queryKey, context?.previousEnvironments)
      queryClient.setQueryData(
        projectKeys.bootstrap(projectId),
        context?.previousBootstrap,
      )
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

function updateProjectEnvironmentCaches(
  queryClient: ReturnType<typeof useQueryClient>,
  projectId: string,
  update: (environments: ProjectEnvironment[]) => ProjectEnvironment[],
) {
  queryClient.setQueryData<ProjectEnvironment[]>(
    projectKeys.environments(projectId),
    (environments) =>
      environments === undefined ? undefined : update(environments),
  )
  queryClient.setQueryData<ProjectBootstrap>(
    projectKeys.bootstrap(projectId),
    (bootstrap) =>
      bootstrap === undefined
        ? undefined
        : {
            ...bootstrap,
            project_environments: update(bootstrap.project_environments),
          },
  )
}
