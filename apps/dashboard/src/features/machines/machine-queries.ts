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

export type MachineDaemon = {
  machine_id: string
  environment_id: string
  name: string
  connector: 'machine_daemon'
  online: boolean
  descriptor: {
    operating_system: OperatingSystem
    architecture: string
  }
  created_at: number
  updated_at: number
  last_seen_at?: number
}

export type MachineInventory = {
  connector_accounts: SandboxConnectorAccount[]
  machine_daemons: MachineDaemon[]
}

export type MachineTarget =
  | { kind: 'tunnel'; machine: MachineDaemon }
  | { kind: 'sandbox'; account: SandboxConnectorAccount }

export type CreateSandboxAccountInput = {
  provider: SandboxProvider
  name: string
  apiKey: string
}

type CreateSandboxAccountResponse = {
  account: SandboxConnectorAccount
}

export const machineKeys = {
  all: ['machines'] as const,
  inventory: () => [...machineKeys.all, 'inventory'] as const,
}

export function useMachineInventory() {
  return useQuery({
    queryKey: machineKeys.inventory(),
    queryFn: ({ signal }) => getJson<MachineInventory>('/api/machines', signal),
    refetchInterval: 15_000,
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
