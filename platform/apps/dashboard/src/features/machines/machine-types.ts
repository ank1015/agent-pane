export type ExecutionHostKind = 'e2b' | 'registered'

export type ExecutionHostState =
  | 'provisioning'
  | 'ready'
  | 'pausing'
  | 'paused'
  | 'resuming'
  | 'unavailable'
  | 'deleting'
  | 'deleted'
  | 'failed'
  | 'lost'

type OperatingSystem =
  | { type: 'linux' | 'macos' | 'windows' | 'free_bsd' }
  | { type: 'other'; name: string }

export type ExecutionHost = {
  id: string
  kind: ExecutionHostKind
  name: string | null
  state: ExecutionHostState
  descriptor: {
    operating_system: OperatingSystem
  } | null
}

export type MachineInventory = {
  registeredHosts: ExecutionHost[]
}

export type E2bAccount = {
  id: string
  name: string
  status: 'active' | 'invalid' | 'disabled'
  is_default: boolean
}

export type CreateE2bAccountInput = { name: string; api_key: string }
