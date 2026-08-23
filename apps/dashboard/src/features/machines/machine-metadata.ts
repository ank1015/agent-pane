import {
  AiCloudIcon,
  CloudServerIcon,
  CodesandboxIcon,
  ComputerCloudIcon,
} from '@hugeicons/core-free-icons'
import type { IconSvgElement } from '@hugeicons/react'
import type { SandboxProvider } from './machine-queries'

export type MachineOption = {
  id: SandboxProvider
  label: string
  icon: IconSvgElement
}

export const SANDBOX_OPTIONS: MachineOption[] = [
  { id: 'e2b', label: 'E2B', icon: CodesandboxIcon },
  { id: 'tensorlake', label: 'Tensorlake', icon: AiCloudIcon },
  { id: 'blaxel', label: 'Blaxel', icon: CloudServerIcon },
  { id: 'daytona', label: 'Daytona', icon: ComputerCloudIcon },
]

export const SANDBOX_PROVIDER_LABELS: Record<SandboxProvider, string> = {
  e2b: 'E2B',
  tensorlake: 'Tensorlake',
  blaxel: 'Blaxel',
  daytona: 'Daytona',
}

export const SANDBOX_PROVIDER_ICONS: Record<
  SandboxProvider,
  IconSvgElement
> = {
  e2b: CodesandboxIcon,
  tensorlake: AiCloudIcon,
  blaxel: CloudServerIcon,
  daytona: ComputerCloudIcon,
}
