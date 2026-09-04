import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getJson, postJson } from '../../lib/api-client'
import type { CreateE2bAccountInput, E2bAccount, ExecutionHost, MachineInventory } from './machine-types'

const MACHINE_REFETCH_INTERVAL_MS = 15_000

export const machineKeys = {
  all: ['machines'] as const,
  inventory: () => [...machineKeys.all, 'inventory'] as const,
  accounts: () => [...machineKeys.all, 'e2b-accounts'] as const,
}

export function useMachineInventory() {
  return useQuery({
    queryKey: machineKeys.inventory(),
    queryFn: ({ signal }) => getJson<ExecutionHost[]>('/api/machines', signal),
    select: groupHosts,
    refetchInterval: MACHINE_REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: false,
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
