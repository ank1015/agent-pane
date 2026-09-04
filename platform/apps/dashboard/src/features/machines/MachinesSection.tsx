import type { ReactNode } from 'react'
import type { ExecutionHost } from './machine-types'
import { useMachineInventory } from './machine-queries'
import { SandboxAccountsSection } from './SandboxAccountsSection'

export function MachinesSection() {
  const { data, error, isError, isPending, refetch } = useMachineInventory()

  return (
    <section className="machines-section" aria-label="Machines">
      <div
        className="machine-inventory-groups"
      >
        <MachineGroup
          heading="Machine Tunnels"
          headingId="machine-tunnels-heading"
          headingLevel="h1"
          hosts={data?.registeredHosts ?? []}
          isPending={isPending}
          isError={isError}
          error={error}
          emptyMessage="No Machine Tunnels"
          loadingMessage="Loading machine tunnels..."
          onRetry={() => void refetch()}
        />

        <SandboxAccountsSection />
      </div>
    </section>
  )
}

function MachineGroup({
  heading,
  headingId,
  headingLevel: Heading,
  hosts,
  isPending,
  isError,
  error,
  emptyMessage,
  loadingMessage,
  onRetry,
}: {
  heading: string
  headingId: string
  headingLevel: 'h1' | 'h2'
  hosts: ExecutionHost[]
  isPending: boolean
  isError: boolean
  error: Error | null
  emptyMessage: string
  loadingMessage: string
  onRetry: () => void
}) {
  return (
    <section className="machine-page-section" aria-labelledby={headingId}>
      <header className="machine-page-section-header">
        <Heading id={headingId} className="cursor-page-title">
          {heading}
        </Heading>
      </header>

      {isPending ? (
        <MachineGroupEmpty>{loadingMessage}</MachineGroupEmpty>
      ) : isError ? (
        <MachineGroupEmpty>
          {error?.message ?? 'The machines could not be loaded.'}
          <button
            type="button"
            className="providers-retry-button"
            onClick={onRetry}
          >
            Retry
          </button>
        </MachineGroupEmpty>
      ) : hosts.length > 0 ? (
        <div className="machine-card-grid">
          {hosts.map((host) => (
            <MachineCard key={host.id} host={host} />
          ))}
        </div>
      ) : (
        <MachineGroupEmpty>{emptyMessage}</MachineGroupEmpty>
      )}
    </section>
  )
}

function MachineCard({ host }: { host: ExecutionHost }) {
  const isActive = host.state === 'ready'
  const statusLabel = isActive ? 'Online' : hostStateLabel(host.state)
  const isSandbox = host.kind === 'e2b'
  const name = host.name ?? (isSandbox ? 'E2B Sandbox' : 'Registered Host')
  const image = machineImage(host)

  return (
    <article
      className="machine-card"
      data-active={isActive ? 'true' : undefined}
      aria-label={`${name}, ${statusLabel}`}
    >
      <div className="machine-card-visual">
        {isSandbox ? null : (
          <span
            className="machine-card-status"
            data-active={isActive ? 'true' : undefined}
            title={statusLabel}
          >
            <span className="machine-card-status-dot" aria-hidden="true" />
          </span>
        )}

        <img
          className="machine-card-image"
          src={image.source}
          data-illustration={isSandbox ? 'logo' : 'machine'}
          data-machine-os={image.operatingSystem}
          alt=""
          aria-hidden="true"
        />
      </div>

      <div className="machine-card-footer">
        <div className="machine-card-identity">
          <div className="machine-card-title-line">
            <h3 title={name}>{name}</h3>
          </div>
        </div>
      </div>
    </article>
  )
}

function MachineGroupEmpty({ children }: { children: ReactNode }) {
  return (
    <div className="machine-inventory-group-empty">
      <div>{children}</div>
    </div>
  )
}

function machineImage(host: ExecutionHost) {
  if (host.kind === 'e2b') {
    return { source: '/e2b.png', operatingSystem: 'e2b' } as const
  }

  switch (host.descriptor?.operating_system.type) {
    case 'linux':
      return { source: '/machine-linux.png', operatingSystem: 'linux' } as const
    case 'macos':
      return { source: '/machine-macos.png', operatingSystem: 'macos' } as const
    case 'windows':
      return {
        source: '/machine-windows-monitor.png',
        operatingSystem: 'windows',
      } as const
    default:
      return { source: '/machine.png', operatingSystem: 'other' } as const
  }
}

function hostStateLabel(state: ExecutionHost['state']) {
  return state
    .split('_')
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}
