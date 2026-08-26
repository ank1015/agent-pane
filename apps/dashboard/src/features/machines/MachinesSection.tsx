import {
  AtIcon,
  Delete03Icon,
  MoreHorizontalIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { createPortal } from 'react-dom'
import { useNavigate } from 'react-router-dom'
import { AddSandboxDialog } from './AddMachineDialog'
import { AddMachineTunnelDialog } from './AddMachineTunnelDialog'
import { DeleteMachineDialog } from './DeleteMachineDialog'
import { UpdateMachineNameDialog } from './UpdateMachineNameDialog'
import {
  type MachineDaemon,
  type MachineTarget,
  type SandboxConnectorAccount,
  useMachineInventory,
} from './machine-queries'

const SANDBOX_PROVIDER_CARD_IMAGES: Partial<
  Record<SandboxConnectorAccount['provider'], string>
> = {
  e2b: '/e2b.png',
  tensorlake: '/tensorlake.png',
  blaxel: '/blaxel.svg',
  daytona: '/daytona.svg',
}

const SANDBOX_PROVIDER_SORT_ORDER: Record<
  SandboxConnectorAccount['provider'],
  number
> = {
  blaxel: 0,
  daytona: 1,
  e2b: 2,
  tensorlake: 3,
}

const CARD_MENU_WIDTH = 208
const CARD_MENU_ESTIMATED_HEIGHT = 92
const CARD_MENU_VIEWPORT_GAP = 8
const CARD_MENU_TRIGGER_GAP = 4

export function MachinesSection() {
  const navigate = useNavigate()
  const [isTunnelDialogOpen, setIsTunnelDialogOpen] = useState(false)
  const [isSandboxDialogOpen, setIsSandboxDialogOpen] = useState(false)
  const [renameTarget, setRenameTarget] = useState<MachineTarget | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<MachineTarget | null>(null)
  const { data, error, isError, isPending, refetch } = useMachineInventory()
  const openTunnelDialog = useCallback(() => setIsTunnelDialogOpen(true), [])
  const closeTunnelDialog = useCallback(
    () => setIsTunnelDialogOpen(false),
    [],
  )
  const openSandboxDialog = useCallback(
    () => setIsSandboxDialogOpen(true),
    [],
  )
  const closeSandboxDialog = useCallback(
    () => setIsSandboxDialogOpen(false),
    [],
  )
  const connectorAccounts = useMemo(
    () =>
      [...(data?.connector_accounts ?? [])].sort(
        (left, right) =>
          SANDBOX_PROVIDER_SORT_ORDER[left.provider] -
            SANDBOX_PROVIDER_SORT_ORDER[right.provider] ||
          left.name.localeCompare(right.name, undefined, {
            sensitivity: 'base',
          }),
      ),
    [data?.connector_accounts],
  )
  const machineDaemons = data?.machine_daemons ?? []

  return (
    <section className="machines-section" aria-label="Machines">
      <div
        className="machine-inventory-groups"
        aria-live="polite"
        aria-busy={isPending}
      >
        <section
          className="machine-page-section"
          aria-labelledby="machine-tunnels-heading"
        >
          <header className="machine-page-section-header">
            <h1 id="machine-tunnels-heading" className="cursor-page-title">
              Machine Tunnels
            </h1>
            <button
              type="button"
              className="cursor-button provider-add-button"
              onClick={openTunnelDialog}
            >
              Add
            </button>
          </header>

          {isPending ? (
            <MachineGroupEmpty>Loading machine tunnels...</MachineGroupEmpty>
          ) : isError ? (
            <MachineGroupEmpty>
              {error.message}
              <button
                type="button"
                className="providers-retry-button"
                onClick={() => void refetch()}
              >
                Retry
              </button>
            </MachineGroupEmpty>
          ) : machineDaemons.length > 0 ? (
            <div className="machine-card-grid">
              {machineDaemons.map((machine) => (
                <MachineDaemonCard
                  key={machine.machine_id}
                  machine={machine}
                  onOpen={() =>
                    navigate(
                      `/machines/${encodeURIComponent(machine.machine_id)}`,
                    )
                  }
                  onUpdateName={() =>
                    setRenameTarget({ kind: 'tunnel', machine })
                  }
                  onDelete={() =>
                    setDeleteTarget({ kind: 'tunnel', machine })
                  }
                />
              ))}
            </div>
          ) : (
            <MachineGroupEmpty>No Machine Tunnels</MachineGroupEmpty>
          )}
        </section>

        <section
          className="machine-page-section"
          aria-labelledby="sandboxes-heading"
        >
          <header className="machine-page-section-header">
            <h2 id="sandboxes-heading" className="cursor-page-title">
              Sandboxes
            </h2>
            <button
              type="button"
              className="cursor-button provider-add-button"
              onClick={openSandboxDialog}
            >
              Add
            </button>
          </header>

          {isPending ? (
            <MachineGroupEmpty>Loading sandboxes...</MachineGroupEmpty>
          ) : isError ? (
            <MachineGroupEmpty>
              {error.message}
              <button
                type="button"
                className="providers-retry-button"
                onClick={() => void refetch()}
              >
                Retry
              </button>
            </MachineGroupEmpty>
          ) : connectorAccounts.length > 0 ? (
            <div className="machine-card-grid">
              {connectorAccounts.map((account) => (
                <ConnectorAccountCard
                  key={account.id}
                  account={account}
                  onUpdateName={() =>
                    setRenameTarget({ kind: 'sandbox', account })
                  }
                  onDelete={() =>
                    setDeleteTarget({ kind: 'sandbox', account })
                  }
                  onOpen={() =>
                    navigate(`/machine/accounts/${encodeURIComponent(account.id)}`)
                  }
                />
              ))}
            </div>
          ) : (
            <MachineGroupEmpty>No Sandboxes</MachineGroupEmpty>
          )}
        </section>
      </div>

      <AddMachineTunnelDialog
        open={isTunnelDialogOpen}
        onClose={closeTunnelDialog}
      />
      <AddSandboxDialog
        open={isSandboxDialogOpen}
        onClose={closeSandboxDialog}
      />
      {renameTarget !== null ? (
        <UpdateMachineNameDialog
          target={renameTarget}
          onClose={() => setRenameTarget(null)}
        />
      ) : null}
      <DeleteMachineDialog
        target={deleteTarget}
        onClose={() => setDeleteTarget(null)}
      />
    </section>
  )
}

function ConnectorAccountCard({
  account,
  onUpdateName,
  onDelete,
  onOpen,
}: {
  account: SandboxConnectorAccount
  onUpdateName: () => void
  onDelete: () => void
  onOpen: () => void
}) {
  return (
    <MachineVisualCard
      name={account.name}
      isActive={account.enabled}
      statusLabel={account.enabled ? 'Enabled' : 'Disabled'}
      provider={account.provider}
      showStatus={false}
      onUpdateName={onUpdateName}
      onDelete={onDelete}
      onActivate={onOpen}
    />
  )
}

function MachineDaemonCard({
  machine,
  onOpen,
  onUpdateName,
  onDelete,
}: {
  machine: MachineDaemon
  onOpen: () => void
  onUpdateName: () => void
  onDelete: () => void
}) {
  return (
    <MachineVisualCard
      name={machine.name}
      isActive={machine.online}
      statusLabel={machine.online ? 'Online' : 'Offline'}
      imageSource={machineImageSource(machine)}
      onActivate={machine.online ? onOpen : undefined}
      onUpdateName={onUpdateName}
      onDelete={onDelete}
    />
  )
}

function MachineVisualCard({
  name,
  isActive,
  statusLabel,
  provider,
  imageSource,
  showStatus = true,
  onUpdateName,
  onDelete,
  onActivate,
}: {
  name: string
  isActive: boolean
  statusLabel: string
  provider?: SandboxConnectorAccount['provider']
  imageSource?: string
  showStatus?: boolean
  onUpdateName: () => void
  onDelete: () => void
  onActivate?: () => void
}) {
  const providerImage =
    provider === undefined ? undefined : SANDBOX_PROVIDER_CARD_IMAGES[provider]
  const resolvedImageSource = imageSource ?? providerImage ?? '/machine.png'

  return (
    <article
      className="machine-card"
      data-active={isActive ? 'true' : undefined}
      data-interactive={onActivate === undefined ? undefined : 'true'}
      aria-label={`${name}, ${statusLabel}`}
      role={onActivate === undefined ? undefined : 'link'}
      tabIndex={onActivate === undefined ? undefined : 0}
      onClick={onActivate}
      onKeyDown={(event) => {
        if (
          onActivate !== undefined &&
          event.target === event.currentTarget &&
          (event.key === 'Enter' || event.key === ' ')
        ) {
          event.preventDefault()
          onActivate()
        }
      }}
    >
      <div className="machine-card-visual">
        {showStatus ? (
          <span
            className="machine-card-status"
            data-active={isActive ? 'true' : undefined}
          >
            <span className="machine-card-status-dot" aria-hidden="true" />
          </span>
        ) : null}

        <img
          className="machine-card-image"
          src={resolvedImageSource}
          data-illustration={providerImage === undefined ? 'machine' : 'logo'}
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
        <MachineCardActionsMenu
          name={name}
          onUpdateName={onUpdateName}
          onDelete={onDelete}
        />
      </div>
    </article>
  )
}

function MachineCardActionsMenu({
  name,
  onUpdateName,
  onDelete,
}: {
  name: string
  onUpdateName: () => void
  onDelete: () => void
}) {
  const [isOpen, setIsOpen] = useState(false)
  const [position, setPosition] = useState({ left: 0, top: 0 })
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const menuId = useId()

  const positionMenu = useCallback(
    (menuHeight = CARD_MENU_ESTIMATED_HEIGHT) => {
      const trigger = triggerRef.current
      if (trigger === null) {
        return
      }

      const bounds = trigger.getBoundingClientRect()
      const left = Math.min(
        Math.max(CARD_MENU_VIEWPORT_GAP, bounds.right - CARD_MENU_WIDTH),
        window.innerWidth - CARD_MENU_WIDTH - CARD_MENU_VIEWPORT_GAP,
      )
      const hasRoomBelow =
        window.innerHeight - bounds.bottom >=
        menuHeight + CARD_MENU_TRIGGER_GAP + CARD_MENU_VIEWPORT_GAP
      const top = hasRoomBelow
        ? bounds.bottom + CARD_MENU_TRIGGER_GAP
        : Math.max(
            CARD_MENU_VIEWPORT_GAP,
            bounds.top - menuHeight - CARD_MENU_TRIGGER_GAP,
          )

      setPosition({ left, top })
    },
    [],
  )

  const closeMenu = useCallback((restoreFocus = false) => {
    setIsOpen(false)
    if (restoreFocus) {
      window.requestAnimationFrame(() => triggerRef.current?.focus())
    }
  }, [])

  useLayoutEffect(() => {
    if (isOpen) {
      positionMenu(menuRef.current?.offsetHeight)
    }
  }, [isOpen, positionMenu])

  useEffect(() => {
    if (!isOpen) {
      return
    }

    const closeOnOutsideClick = (event: PointerEvent) => {
      const target = event.target
      if (
        target instanceof Node &&
        !triggerRef.current?.contains(target) &&
        !menuRef.current?.contains(target)
      ) {
        closeMenu()
      }
    }
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        closeMenu(true)
      }
    }
    const closeOnViewportChange = () => closeMenu()

    document.addEventListener('pointerdown', closeOnOutsideClick)
    document.addEventListener('keydown', closeOnEscape)
    window.addEventListener('resize', closeOnViewportChange)
    window.addEventListener('scroll', closeOnViewportChange, true)
    return () => {
      document.removeEventListener('pointerdown', closeOnOutsideClick)
      document.removeEventListener('keydown', closeOnEscape)
      window.removeEventListener('resize', closeOnViewportChange)
      window.removeEventListener('scroll', closeOnViewportChange, true)
    }
  }, [closeMenu, isOpen])

  const toggleMenu = () => {
    if (!isOpen) {
      positionMenu()
    }
    setIsOpen((open) => !open)
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className="cursor-button cursor-button--ghost cursor-icon-button machine-card-actions"
        aria-label={`Actions for ${name}`}
        aria-haspopup="menu"
        aria-expanded={isOpen}
        aria-controls={isOpen ? menuId : undefined}
        onClick={(event) => {
          event.stopPropagation()
          toggleMenu()
        }}
      >
        <HugeiconsIcon
          icon={MoreHorizontalIcon}
          size={16}
          color="currentColor"
          strokeWidth={1.5}
          aria-hidden="true"
        />
      </button>

      {isOpen
        ? createPortal(
            <div
              ref={menuRef}
              id={menuId}
              className="provider-actions-menu machine-card-actions-menu"
              role="menu"
              aria-label={`Actions for ${name}`}
              style={position}
              onClick={(event) => event.stopPropagation()}
            >
              <MachineCardMenuItem
                icon={AtIcon}
                label="Update Name"
                onClick={() => {
                  closeMenu()
                  onUpdateName()
                }}
              />
              <div className="provider-actions-separator" role="separator" />
              <MachineCardMenuItem
                icon={Delete03Icon}
                label="Delete"
                danger
                onClick={() => {
                  closeMenu()
                  onDelete()
                }}
              />
            </div>,
            document.body,
          )
        : null}
    </>
  )
}

function MachineCardMenuItem({
  danger = false,
  icon,
  label,
  onClick,
}: {
  danger?: boolean
  icon: typeof AtIcon
  label: string
  onClick: () => void
}) {
  return (
    <button
      type="button"
      role="menuitem"
      className={`provider-actions-item${danger ? ' provider-actions-item--danger' : ''}`}
      onClick={onClick}
    >
      <HugeiconsIcon
        icon={icon}
        size={16}
        color="currentColor"
        strokeWidth={1.5}
        aria-hidden="true"
      />
      <span>{label}</span>
    </button>
  )
}

function MachineGroupEmpty({ children }: { children: React.ReactNode }) {
  return (
    <div className="machine-inventory-group-empty">
      <div>{children}</div>
    </div>
  )
}

function machineImageSource(machine: MachineDaemon): string {
  switch (machine.descriptor.operating_system.type) {
    case 'macos':
      return '/machine-macos.png'
    case 'windows':
      return '/machine-windows.png'
    case 'linux':
      return '/machine-linux.png'
    default:
      return '/machine.png'
  }
}
