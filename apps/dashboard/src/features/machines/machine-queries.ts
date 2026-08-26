import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  deleteRequest,
  getJson,
  patchJson,
  postJson,
} from '../../lib/api-client'

export type SandboxProvider = 'e2b' | 'tensorlake' | 'blaxel' | 'daytona'

export type SandboxConnectorAccount = {
  id: string
  provider: SandboxProvider
  name: string
  config: Record<string, unknown>
  enabled: boolean
  is_default: boolean
  validation_status: 'unchecked' | 'valid' | 'invalid'
  last_validated_at?: number
  credential_version: number
  credentials_updated_at: number
  created_at: number
  updated_at: number
}

type OperatingSystem =
  | { type: 'linux' | 'macos' | 'windows' | 'free_bsd' }
  | { type: 'other'; name: string }

type WorkspaceRoot = {
  id: string
  name: string
  uri: string
  read_only: boolean
}

export type MachineDaemon = {
  machine_id: string
  name: string
  connector: 'machine_daemon'
  online: boolean
  descriptor: {
    operating_system: OperatingSystem
    architecture: string
    path_convention: 'posix' | 'windows'
    workspace_roots: WorkspaceRoot[]
  }
  created_at: number
  updated_at: number
  last_seen_at?: number
}

export type MachineInventory = {
  connector_accounts: SandboxConnectorAccount[]
  machine_daemons: MachineDaemon[]
}

export type MachineEnvironment = {
  environment_id: string
  machine_id: string
  name: string
  workspace_root_id: string
  path: string
  created_at: number
}

export type MachineTarget =
  | { kind: 'tunnel'; machine: MachineDaemon }
  | { kind: 'sandbox'; account: SandboxConnectorAccount }

export type CreateSandboxAccountInput = {
  provider: SandboxProvider
  name: string
  apiKey: string
}

export type CreateMachineEnvironmentInput = {
  machineId: string
  name: string
  workspaceRootId: string
  path: string
}

export type UpdateMachineEnvironmentNameInput = {
  environment: MachineEnvironment
  name: string
}

type CreateSandboxAccountResponse = {
  account: SandboxConnectorAccount
}

export const machineKeys = {
  all: ['machines'] as const,
  inventory: () => [...machineKeys.all, 'inventory'] as const,
  environments: (machineId: string) =>
    [...machineKeys.all, machineId, 'environments'] as const,
}

export function useMachineInventory() {
  return useQuery({
    queryKey: machineKeys.inventory(),
    queryFn: ({ signal }) => getJson<MachineInventory>('/api/machines', signal),
    refetchInterval: 15_000,
  })
}

export function useMachineEnvironments(machineId: string) {
  return useQuery({
    queryKey: machineKeys.environments(machineId),
    queryFn: ({ signal }) =>
      getJson<MachineEnvironment[]>(
        `/api/machines/${encodeURIComponent(machineId)}/environments`,
        signal,
      ),
    enabled: machineId.length > 0,
  })
}

export function useCreateMachineEnvironment() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({
      machineId,
      name,
      workspaceRootId,
      path,
    }: CreateMachineEnvironmentInput) =>
      postJson<MachineEnvironment>(
        `/api/machines/${encodeURIComponent(machineId)}/environments`,
        {
          name,
          workspace_root_id: workspaceRootId,
          path,
        },
      ),
    onSuccess: (environment) => {
      queryClient.setQueryData<MachineEnvironment[]>(
        machineKeys.environments(environment.machine_id),
        (environments) =>
          environments === undefined
            ? [environment]
            : [...environments, environment],
      )
      return queryClient.invalidateQueries({
        queryKey: machineKeys.environments(environment.machine_id),
      })
    },
  })
}

export function useUpdateMachineEnvironmentName() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({
      environment,
      name,
    }: UpdateMachineEnvironmentNameInput) =>
      patchJson<MachineEnvironment>(
        `/api/machines/environments/${encodeURIComponent(environment.environment_id)}`,
        { name },
      ),
    onMutate: async ({ environment, name }) => {
      const queryKey = machineKeys.environments(environment.machine_id)
      await queryClient.cancelQueries({ queryKey })
      const previousEnvironments =
        queryClient.getQueryData<MachineEnvironment[]>(queryKey)
      queryClient.setQueryData<MachineEnvironment[]>(queryKey, (environments) =>
        environments?.map((candidate) =>
          candidate.environment_id === environment.environment_id
            ? { ...candidate, name }
            : candidate,
        ),
      )
      return { previousEnvironments, queryKey }
    },
    onError: (_error, { environment }, context) => {
      queryClient.setQueryData(
        machineKeys.environments(environment.machine_id),
        context?.previousEnvironments,
      )
    },
    onSettled: (_data, _error, { environment }) =>
      queryClient.invalidateQueries({
        queryKey: machineKeys.environments(environment.machine_id),
      }),
  })
}

export function useDeleteMachineEnvironment() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: (environment: MachineEnvironment) =>
      deleteRequest(
        `/api/machines/environments/${encodeURIComponent(environment.environment_id)}`,
      ),
    onMutate: async (environment) => {
      const queryKey = machineKeys.environments(environment.machine_id)
      await queryClient.cancelQueries({ queryKey })
      const previousEnvironments =
        queryClient.getQueryData<MachineEnvironment[]>(queryKey)
      queryClient.setQueryData<MachineEnvironment[]>(queryKey, (environments) =>
        environments?.filter(
          (candidate) =>
            candidate.environment_id !== environment.environment_id,
        ),
      )
      return { previousEnvironments, queryKey }
    },
    onError: (_error, environment, context) => {
      queryClient.setQueryData(
        machineKeys.environments(environment.machine_id),
        context?.previousEnvironments,
      )
    },
    onSettled: (_data, _error, environment) =>
      queryClient.invalidateQueries({
        queryKey: machineKeys.environments(environment.machine_id),
      }),
  })
}

export function useCreateSandboxAccount() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ provider, name, apiKey }: CreateSandboxAccountInput) =>
      postJson<CreateSandboxAccountResponse>(
        '/api/machines/sandbox-accounts',
        {
          provider,
          name,
          api_key: apiKey,
        },
      ),
    onSuccess: ({ account }) => {
      queryClient.setQueryData<MachineInventory>(
        machineKeys.inventory(),
        (inventory) =>
          inventory === undefined
            ? inventory
            : {
                ...inventory,
                connector_accounts: [...inventory.connector_accounts, account],
              },
      )
      return queryClient.invalidateQueries({ queryKey: machineKeys.all })
    },
  })
}

export function useUpdateMachineName() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async ({
      target,
      name,
    }: {
      target: MachineTarget
      name: string
    }) => {
      if (target.kind === 'tunnel') {
        await patchJson<{ machine: MachineDaemon }>(
          `/api/machines/tunnels/${encodeURIComponent(target.machine.machine_id)}`,
          { name },
        )
        return
      }
      await patchJson<{ account: SandboxConnectorAccount }>(
        `/api/machines/sandbox-accounts/${encodeURIComponent(target.account.id)}`,
        { name },
      )
    },
    onMutate: async ({ target, name }) => {
      await queryClient.cancelQueries({ queryKey: machineKeys.inventory() })
      const previousInventory = queryClient.getQueryData<MachineInventory>(
        machineKeys.inventory(),
      )
      queryClient.setQueryData<MachineInventory>(
        machineKeys.inventory(),
        (inventory) => {
          if (inventory === undefined) {
            return inventory
          }
          return target.kind === 'tunnel'
            ? {
                ...inventory,
                machine_daemons: inventory.machine_daemons.map((machine) =>
                  machine.machine_id === target.machine.machine_id
                    ? { ...machine, name }
                    : machine,
                ),
              }
            : {
                ...inventory,
                connector_accounts: inventory.connector_accounts.map(
                  (account) =>
                    account.id === target.account.id
                      ? { ...account, name }
                      : account,
                ),
              }
        },
      )
      return { previousInventory }
    },
    onError: (_error, _variables, context) => {
      queryClient.setQueryData(
        machineKeys.inventory(),
        context?.previousInventory,
      )
    },
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: machineKeys.inventory() }),
  })
}

export function useDeleteMachine() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: (target: MachineTarget) =>
      deleteRequest(
        target.kind === 'tunnel'
          ? `/api/machines/tunnels/${encodeURIComponent(target.machine.machine_id)}`
          : `/api/machines/sandbox-accounts/${encodeURIComponent(target.account.id)}`,
      ),
    onMutate: async (target) => {
      await queryClient.cancelQueries({ queryKey: machineKeys.inventory() })
      const previousInventory = queryClient.getQueryData<MachineInventory>(
        machineKeys.inventory(),
      )
      queryClient.setQueryData<MachineInventory>(
        machineKeys.inventory(),
        (inventory) => {
          if (inventory === undefined) {
            return inventory
          }
          return target.kind === 'tunnel'
            ? {
                ...inventory,
                machine_daemons: inventory.machine_daemons.filter(
                  (machine) =>
                    machine.machine_id !== target.machine.machine_id,
                ),
              }
            : {
                ...inventory,
                connector_accounts: inventory.connector_accounts.filter(
                  (account) => account.id !== target.account.id,
                ),
              }
        },
      )
      return { previousInventory }
    },
    onError: (_error, _target, context) => {
      queryClient.setQueryData(
        machineKeys.inventory(),
        context?.previousInventory,
      )
    },
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: machineKeys.inventory() }),
  })
}
