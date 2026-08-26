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

export type SandboxMachine = {
  machine_id: string
  sandbox_account_id: string
  sandbox_id: string
  name: string
  created_from: string | null
  created_at: number
}

export type SandboxSnapshot = {
  id: string
  name: string
  sandbox_account_id: string
  provider: SandboxProvider
  provider_snapshot_id: string
  sandbox_id: string
  created_at: number
}

export type SandboxEnvironmentTemplate = {
  id: string
  name: string
  snapshot_id: string
  sandbox_account_id: string
  provider: SandboxProvider
  cwd: string
  creation_script: string
  created_at: number
  updated_at: number
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

export type CreateE2bSandboxInput = {
  accountId: string
  name: string
  templateId?: string
}

export type CreateE2bSnapshotInput = {
  accountId: string
  sandboxId: string
  name: string
}

export type CreateSandboxEnvironmentTemplateInput = {
  accountId: string
  name: string
  snapshotId: string
  path: string
  setupScript: string
}

export type UpdateMachineEnvironmentNameInput = {
  environment: MachineEnvironment
  name: string
}

export type UpdateSandboxMachineNameInput = {
  sandbox: SandboxMachine
  name: string
}

export type UpdateSnapshotNameInput = {
  snapshot: SandboxSnapshot
  name: string
}

export type UpdateSandboxEnvironmentTemplateNameInput = {
  template: SandboxEnvironmentTemplate
  name: string
}

type CreateSandboxAccountResponse = {
  account: SandboxConnectorAccount
}

type MachineNameUpdatedResponse = {
  machine: {
    machine_id: string
    name: string
  }
}

type E2bSandboxCreated = {
  machine: {
    machine_id: string
    name: string
    created_at: number
  }
  sandbox_account_id: string
  sandbox_id: string
  template_id: string
}

export const machineKeys = {
  all: ['machines'] as const,
  inventory: () => [...machineKeys.all, 'inventory'] as const,
  environments: (machineId: string) =>
    [...machineKeys.all, machineId, 'environments'] as const,
  sandboxes: (accountId: string) =>
    [...machineKeys.all, 'sandbox-accounts', accountId, 'sandboxes'] as const,
  snapshots: (accountId: string) =>
    [...machineKeys.all, 'sandbox-accounts', accountId, 'snapshots'] as const,
  environmentTemplates: (accountId: string) =>
    [
      ...machineKeys.all,
      'sandbox-accounts',
      accountId,
      'environment-templates',
    ] as const,
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

export function useSandboxMachines(accountId: string, enabled = true) {
  return useQuery({
    queryKey: machineKeys.sandboxes(accountId),
    queryFn: ({ signal }) =>
      getJson<SandboxMachine[]>(
        `/api/machines/sandbox-accounts/${encodeURIComponent(accountId)}/sandboxes`,
        signal,
      ),
    enabled: enabled && accountId.length > 0,
  })
}

export function useSandboxSnapshots(accountId: string) {
  return useQuery({
    queryKey: machineKeys.snapshots(accountId),
    queryFn: ({ signal }) =>
      getJson<SandboxSnapshot[]>(
        `/api/machines/snapshots?sandbox_account_id=${encodeURIComponent(accountId)}`,
        signal,
      ),
    enabled: accountId.length > 0,
  })
}

export function useSandboxEnvironmentTemplates(accountId: string) {
  return useQuery({
    queryKey: machineKeys.environmentTemplates(accountId),
    queryFn: async ({ signal }) => {
      const templates = await getJson<SandboxEnvironmentTemplate[]>(
        '/api/machines/sandbox-environment-templates',
        signal,
      )
      return templates.filter(
        (template) => template.sandbox_account_id === accountId,
      )
    },
    enabled: accountId.length > 0,
  })
}

export function useCreateE2bSandbox() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ accountId, name, templateId }: CreateE2bSandboxInput) =>
      postJson<E2bSandboxCreated>(
        `/api/machines/sandbox-accounts/${encodeURIComponent(accountId)}/sandboxes`,
        {
          name,
          ...(templateId === undefined ? {} : { template_id: templateId }),
        },
      ),
    onSuccess: (created) => {
      const sandbox: SandboxMachine = {
        machine_id: created.machine.machine_id,
        sandbox_account_id: created.sandbox_account_id,
        sandbox_id: created.sandbox_id,
        name: created.machine.name,
        created_from: created.template_id,
        created_at: created.machine.created_at,
      }
      const queryKey = machineKeys.sandboxes(created.sandbox_account_id)
      queryClient.setQueryData<SandboxMachine[]>(queryKey, (sandboxes) =>
        sandboxes === undefined ? [sandbox] : [sandbox, ...sandboxes],
      )
      return queryClient.invalidateQueries({ queryKey })
    },
  })
}

export function useCreateE2bSnapshot() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({
      accountId,
      sandboxId,
      name,
    }: CreateE2bSnapshotInput) =>
      postJson<SandboxSnapshot>(
        `/api/machines/sandbox-accounts/${encodeURIComponent(accountId)}/sandboxes/${encodeURIComponent(sandboxId)}/snapshots`,
        { name },
      ),
    onSuccess: (snapshot) => {
      const queryKey = machineKeys.snapshots(snapshot.sandbox_account_id)
      queryClient.setQueryData<SandboxSnapshot[]>(queryKey, (snapshots) =>
        snapshots === undefined ? [snapshot] : [snapshot, ...snapshots],
      )
      return queryClient.invalidateQueries({ queryKey })
    },
  })
}

export function useCreateSandboxEnvironmentTemplate() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({
      name,
      snapshotId,
      path,
      setupScript,
    }: CreateSandboxEnvironmentTemplateInput) =>
      postJson<SandboxEnvironmentTemplate>(
        '/api/machines/sandbox-environment-templates',
        {
          name,
          snapshot_id: snapshotId,
          cwd: path,
          creation_script: setupScript,
        },
      ),
    onSuccess: (template) => {
      const queryKey = machineKeys.environmentTemplates(
        template.sandbox_account_id,
      )
      queryClient.setQueryData<SandboxEnvironmentTemplate[]>(
        queryKey,
        (templates) =>
          templates === undefined ? [template] : [...templates, template],
      )
      return queryClient.invalidateQueries({ queryKey })
    },
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

export function useUpdateSandboxMachineName() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ sandbox, name }: UpdateSandboxMachineNameInput) =>
      patchJson<MachineNameUpdatedResponse>(
        `/api/machines/${encodeURIComponent(sandbox.machine_id)}`,
        { name },
      ),
    onMutate: async ({ sandbox, name }) => {
      const queryKey = machineKeys.sandboxes(sandbox.sandbox_account_id)
      await queryClient.cancelQueries({ queryKey })
      const previousSandboxes =
        queryClient.getQueryData<SandboxMachine[]>(queryKey)
      queryClient.setQueryData<SandboxMachine[]>(queryKey, (sandboxes) =>
        sandboxes?.map((candidate) =>
          candidate.machine_id === sandbox.machine_id
            ? { ...candidate, name }
            : candidate,
        ),
      )
      return { previousSandboxes, queryKey }
    },
    onError: (_error, { sandbox }, context) => {
      queryClient.setQueryData(
        machineKeys.sandboxes(sandbox.sandbox_account_id),
        context?.previousSandboxes,
      )
    },
    onSettled: (_data, _error, { sandbox }) =>
      queryClient.invalidateQueries({
        queryKey: machineKeys.sandboxes(sandbox.sandbox_account_id),
      }),
  })
}

export function useUpdateSnapshotName() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ snapshot, name }: UpdateSnapshotNameInput) =>
      patchJson<SandboxSnapshot>(
        `/api/machines/snapshots/${encodeURIComponent(snapshot.id)}`,
        { name },
      ),
    onMutate: async ({ snapshot, name }) => {
      const queryKey = machineKeys.snapshots(snapshot.sandbox_account_id)
      await queryClient.cancelQueries({ queryKey })
      const previousSnapshots =
        queryClient.getQueryData<SandboxSnapshot[]>(queryKey)
      queryClient.setQueryData<SandboxSnapshot[]>(queryKey, (snapshots) =>
        snapshots?.map((candidate) =>
          candidate.id === snapshot.id ? { ...candidate, name } : candidate,
        ),
      )
      return { previousSnapshots }
    },
    onError: (_error, { snapshot }, context) => {
      queryClient.setQueryData(
        machineKeys.snapshots(snapshot.sandbox_account_id),
        context?.previousSnapshots,
      )
    },
    onSettled: (_data, _error, { snapshot }) =>
      queryClient.invalidateQueries({
        queryKey: machineKeys.snapshots(snapshot.sandbox_account_id),
      }),
  })
}

export function useUpdateSandboxEnvironmentTemplateName() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({
      template,
      name,
    }: UpdateSandboxEnvironmentTemplateNameInput) =>
      patchJson<SandboxEnvironmentTemplate>(
        `/api/machines/sandbox-environment-templates/${encodeURIComponent(template.id)}`,
        { name },
      ),
    onMutate: async ({ template, name }) => {
      const queryKey = machineKeys.environmentTemplates(
        template.sandbox_account_id,
      )
      await queryClient.cancelQueries({ queryKey })
      const previousTemplates =
        queryClient.getQueryData<SandboxEnvironmentTemplate[]>(queryKey)
      queryClient.setQueryData<SandboxEnvironmentTemplate[]>(
        queryKey,
        (templates) =>
          templates?.map((candidate) =>
            candidate.id === template.id ? { ...candidate, name } : candidate,
          ),
      )
      return { previousTemplates }
    },
    onError: (_error, { template }, context) => {
      queryClient.setQueryData(
        machineKeys.environmentTemplates(template.sandbox_account_id),
        context?.previousTemplates,
      )
    },
    onSettled: (_data, _error, { template }) =>
      queryClient.invalidateQueries({
        queryKey: machineKeys.environmentTemplates(
          template.sandbox_account_id,
        ),
      }),
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
