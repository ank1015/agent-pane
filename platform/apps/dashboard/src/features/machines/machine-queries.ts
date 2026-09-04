import { useIsMutating, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { deleteRequest, getJson, patchJson, postJson } from '../../lib/api-client'
import type { CreateE2bAccountInput, E2bAccount, ExecutionHost, MachineInventory } from './machine-types'

const MACHINE_REFETCH_INTERVAL_MS = 15_000

export const machineKeys = {
  all: ['machines'] as const,
  inventory: () => [...machineKeys.all, 'inventory'] as const,
  accounts: () => [...machineKeys.all, 'e2b-accounts'] as const,
  actions: () => [...machineKeys.all, 'actions'] as const,
}

export function useMachineInventory() {
  const isChanging = useIsMutating({ mutationKey: machineKeys.actions() }) > 0
  return useQuery({
    queryKey: machineKeys.inventory(),
    queryFn: ({ signal }) => getJson<ExecutionHost[]>('/api/machines', signal),
    select: groupHosts,
    refetchInterval: MACHINE_REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: false,
    enabled: !isChanging,
  })
}

export function useMachineAction() {
  const client = useQueryClient()
  const queryKey = machineKeys.inventory()
  return useMutation({
    mutationKey: machineKeys.actions(),
    retry: false,
    gcTime: 0,
    mutationFn: async (input: { host: ExecutionHost; action: 'rename'; name: string } | { host: ExecutionHost; action: 'delete' }) => {
      const path = `/api/machines/${encodeURIComponent(input.host.id)}`
      if (input.action === 'rename') return patchJson<ExecutionHost>(path, { name: input.name })
      await deleteRequest(path)
      return null
    },
    onMutate: async (input) => {
      await client.cancelQueries({ queryKey })
      const previous = client.getQueryData<ExecutionHost[]>(queryKey)?.find((host) => host.id === input.host.id)
      client.setQueryData<ExecutionHost[]>(queryKey, (hosts) => input.action === 'delete'
        ? hosts?.filter((host) => host.id !== input.host.id)
        : hosts?.map((host) => host.id === input.host.id ? { ...host, name: input.name } : host))
      return { previous }
    },
    onError: (_error, input, context) => {
      const previous = context?.previous
      if (!previous) return
      // Restore only this host, never overwrite changes to other machines.
      client.setQueryData<ExecutionHost[]>(queryKey, (hosts) => {
        if (!hosts) return hosts
        return hosts.some((host) => host.id === input.host.id)
          ? hosts.map((host) => host.id === input.host.id ? previous : host)
          : [...hosts, previous]
      })
    },
    onSuccess: (host) => {
      if (host) client.setQueryData<ExecutionHost[]>(queryKey, (hosts) => hosts?.map((item) => item.id === host.id ? host : item))
    },
    onSettled: () => { void client.invalidateQueries({ queryKey }) },
  })
}

function groupHosts(hosts: ExecutionHost[]): MachineInventory {
  return {
    registeredHosts: hosts.filter((host) => host.kind === 'registered'),
  }
}

const accountsPath = '/api/machines/e2b-accounts'

export function useE2bAccounts() {
  return useQuery({
    queryKey: machineKeys.accounts(),
    queryFn: ({ signal }) => getJson<E2bAccount[]>(accountsPath, signal),
    refetchInterval: MACHINE_REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: false,
  })
}

export function useCreateE2bAccount() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (input: CreateE2bAccountInput) => postJson<E2bAccount>(accountsPath, input),
    retry: false,
    gcTime: 0,
    onSuccess: async (account) => {
      await client.cancelQueries({ queryKey: machineKeys.accounts() })
      client.setQueryData<E2bAccount[]>(machineKeys.accounts(), (previous = []) => [
        ...previous.filter((existing) => existing.id !== account.id),
        account,
      ])
      // A failed refresh must not turn a successful creation into a failed submission.
      void client.invalidateQueries({ queryKey: machineKeys.accounts() })
    },
    onSettled: (_data, _error, input) => {
      // Mutation variables otherwise remain in the in-memory mutation cache.
      input.api_key = ''
    },
  })
}
