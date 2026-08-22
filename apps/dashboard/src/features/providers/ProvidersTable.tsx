import {
  ArrowUpBigIcon,
  AtIcon,
  Delete03Icon,
  MoreHorizontalIcon,
  Settings01Icon,
  ToggleOffIcon,
  ToggleOnIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import { createPortal } from 'react-dom'
import { AddProviderDialog } from './AddProviderDialog'
import { DeleteProviderDialog } from './DeleteProviderDialog'
import { ProviderIcon } from './provider-icons'
import { PROVIDER_LABELS } from './provider-metadata'
import { UpdateProviderNameDialog } from './UpdateProviderNameDialog'
import {
  type ProviderAccount,
  useProviderAccounts,
  useSetDefaultProviderAccount,
  useSetProviderAccountEnabled,
} from './provider-queries'

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
})

export function ProvidersTable() {
  const [isAddDialogOpen, setIsAddDialogOpen] = useState(false)
  const [accountToDelete, setAccountToDelete] = useState<ProviderAccount | null>(
    null,
  )
  const [accountToRename, setAccountToRename] = useState<ProviderAccount | null>(
    null,
  )
  const { data, error, isError, isPending, refetch } = useProviderAccounts()
  const openAddDialog = useCallback(() => setIsAddDialogOpen(true), [])
  const closeAddDialog = useCallback(() => setIsAddDialogOpen(false), [])
  const openDeleteDialog = useCallback(
    (account: ProviderAccount) => setAccountToDelete(account),
    [],
  )
  const closeDeleteDialog = useCallback(() => setAccountToDelete(null), [])
  const openUpdateNameDialog = useCallback(
    (account: ProviderAccount) => setAccountToRename(account),
    [],
  )
  const closeUpdateNameDialog = useCallback(() => setAccountToRename(null), [])

  return (
    <section
      className="providers-section"
      aria-labelledby="configured-providers-title"
    >
      <div className="providers-section-header">
        <h2 id="configured-providers-title">User Configured Providers</h2>
        <button
          type="button"
          className="cursor-button provider-add-button"
          onClick={openAddDialog}
        >
          Add
        </button>
      </div>

      <div className="providers-table-wrap">
        <table className="providers-table">
          <colgroup>
            <col className="provider-icon-column" />
            <col className="provider-name-column" />
            <col className="provider-type-column" />
            <col className="provider-status-column" />
            <col className="provider-added-column" />
            <col className="provider-actions-column" />
          </colgroup>
          <thead>
            <tr>
              <th scope="col">
                <span className="visually-hidden">Provider icon</span>
              </th>
              <th scope="col">Name</th>
              <th scope="col">Provider</th>
              <th scope="col">Status</th>
              <th scope="col">Added</th>
              <th scope="col">
                <span className="visually-hidden">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody aria-live="polite" aria-busy={isPending}>
            {isPending ? <TableMessage>Loading providers...</TableMessage> : null}
            {isError ? (
              <TableError message={error.message} onRetry={() => void refetch()} />
            ) : null}
            {!isPending && !isError && data?.length === 0 ? (
              <TableMessage>No providers added</TableMessage>
            ) : null}
            {!isPending && !isError
              ? data?.map((account) => (
                  <ProviderRow
                    key={account.id}
                    account={account}
                    onDelete={openDeleteDialog}
                    onUpdateName={openUpdateNameDialog}
                  />
                ))
              : null}
          </tbody>
        </table>
      </div>

      <AddProviderDialog
        open={isAddDialogOpen}
        onClose={closeAddDialog}
      />
      <DeleteProviderDialog
        account={accountToDelete}
        onClose={closeDeleteDialog}
      />
      {accountToRename !== null ? (
        <UpdateProviderNameDialog
          key={accountToRename.id}
          account={accountToRename}
          onClose={closeUpdateNameDialog}
        />
      ) : null}
    </section>
  )
}

function ProviderRow({
  account,
  onDelete,
  onUpdateName,
}: {
  account: ProviderAccount
  onDelete: (account: ProviderAccount) => void
  onUpdateName: (account: ProviderAccount) => void
}) {
  return (
    <tr>
      <td className="provider-icon-cell">
        <ProviderIcon provider={account.provider} width={16} height={16} />
      </td>
      <td>
        <span className="provider-name-line">
          <span className="provider-name">{account.name}</span>
          {account.is_default ? (
            <span className="provider-default-badge">Default</span>
          ) : null}
        </span>
      </td>
      <td>
        <span className="provider-detail">
          {PROVIDER_LABELS[account.provider]}
        </span>
      </td>
      <td>
        <span
          className={`provider-detail provider-status provider-status--${account.status}`}
        >
          {account.status === 'enabled' ? 'Enabled' : 'Disabled'}
        </span>
      </td>
      <td>
        <time className="provider-detail" dateTime={account.created_at}>
          {formatCreatedAt(account.created_at)}
        </time>
      </td>
      <td className="provider-actions-cell">
        <ProviderActionsMenu
          account={account}
          onDelete={() => onDelete(account)}
          onUpdateName={() => onUpdateName(account)}
        />
      </td>
    </tr>
  )
}

const PROVIDER_MENU_WIDTH = 208
const PROVIDER_MENU_ESTIMATED_HEIGHT = 186
const PROVIDER_MENU_VIEWPORT_GAP = 8
const PROVIDER_MENU_TRIGGER_GAP = 4

function ProviderActionsMenu({
  account,
  onDelete,
  onUpdateName,
}: {
  account: ProviderAccount
  onDelete: () => void
  onUpdateName: () => void
}) {
  const [isOpen, setIsOpen] = useState(false)
  const [position, setPosition] = useState({ left: 0, top: 0 })
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const menuId = useId()
  const isEnabled = account.status === 'enabled'
  const setDefaultProvider = useSetDefaultProviderAccount()
  const setProviderEnabled = useSetProviderAccountEnabled()
  const isActionPending =
    setDefaultProvider.isPending || setProviderEnabled.isPending

  const positionMenu = useCallback((menuHeight = PROVIDER_MENU_ESTIMATED_HEIGHT) => {
    const trigger = triggerRef.current
    if (trigger === null) {
      return
    }

    const bounds = trigger.getBoundingClientRect()
    const left = Math.min(
      Math.max(
        PROVIDER_MENU_VIEWPORT_GAP,
        bounds.right - PROVIDER_MENU_WIDTH,
      ),
      window.innerWidth - PROVIDER_MENU_WIDTH - PROVIDER_MENU_VIEWPORT_GAP,
    )
    const hasRoomBelow =
      window.innerHeight - bounds.bottom >=
      menuHeight + PROVIDER_MENU_TRIGGER_GAP + PROVIDER_MENU_VIEWPORT_GAP
    const top = hasRoomBelow
      ? bounds.bottom + PROVIDER_MENU_TRIGGER_GAP
      : Math.max(
          PROVIDER_MENU_VIEWPORT_GAP,
          bounds.top - menuHeight - PROVIDER_MENU_TRIGGER_GAP,
        )

    setPosition({ left, top })
  }, [])

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
  }, [
    isOpen,
    positionMenu,
    setDefaultProvider.isError,
    setProviderEnabled.isError,
  ])

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
      setDefaultProvider.reset()
      setProviderEnabled.reset()
      positionMenu()
    }
    setIsOpen((open) => !open)
  }

  const dismissMenuItem = () => closeMenu(true)
  const openDeleteDialog = () => {
    closeMenu()
    onDelete()
  }
  const openUpdateNameDialog = () => {
    closeMenu()
    onUpdateName()
  }
  const toggleEnabled = async () => {
    try {
      await setProviderEnabled.mutateAsync({
        providerId: account.id,
        enabled: !isEnabled,
      })
      closeMenu(true)
    } catch {
      // The mutation state renders the platform or gateway error in the menu.
    }
  }
  const setAsDefault = async () => {
    try {
      await setDefaultProvider.mutateAsync(account.id)
      closeMenu(true)
    } catch {
      // The mutation state renders the platform or gateway error in the menu.
    }
  }
  const cannotDisableDefault = isEnabled && account.is_default
  const cannotSetDefault = account.is_default || !isEnabled

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className="cursor-button cursor-button--ghost cursor-icon-button provider-actions-trigger"
        aria-label={`Actions for ${account.name}`}
        aria-haspopup="menu"
        aria-expanded={isOpen}
        aria-controls={isOpen ? menuId : undefined}
        onClick={toggleMenu}
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
              className="provider-actions-menu"
              role="menu"
              aria-label={`Actions for ${account.name}`}
              style={position}
            >
              <ProviderMenuItem
                icon={AtIcon}
                label="Update name"
                disabled={isActionPending}
                onClick={openUpdateNameDialog}
              />
              <ProviderMenuItem
                icon={Settings01Icon}
                label="Update Configuration"
                disabled={isActionPending}
                onClick={dismissMenuItem}
              />
              <ProviderMenuItem
                icon={ArrowUpBigIcon}
                label={
                  setDefaultProvider.isPending ? 'Setting default…' : 'Set Default'
                }
                disabled={cannotSetDefault || isActionPending}
                title={
                  !isEnabled
                    ? 'Enable this account before setting it as default'
                    : undefined
                }
                onClick={() => void setAsDefault()}
              />
              <ProviderMenuItem
                icon={isEnabled ? ToggleOffIcon : ToggleOnIcon}
                label={
                  setProviderEnabled.isPending
                    ? isEnabled
                      ? 'Disabling…'
                      : 'Enabling…'
                    : isEnabled
                      ? 'Disable'
                      : 'Enable'
                }
                disabled={isActionPending || cannotDisableDefault}
                title={
                  cannotDisableDefault
                    ? 'Set another default before disabling this account'
                    : undefined
                }
                onClick={() => void toggleEnabled()}
              />
              {setProviderEnabled.isError ? (
                <p className="provider-actions-error" role="alert">
                  {setProviderEnabled.error.message}
                </p>
              ) : null}
              {setDefaultProvider.isError ? (
                <p className="provider-actions-error" role="alert">
                  {setDefaultProvider.error.message}
                </p>
              ) : null}
              <div className="provider-actions-separator" role="separator" />
              <ProviderMenuItem
                icon={Delete03Icon}
                label="Delete"
                danger
                disabled={isActionPending}
                onClick={openDeleteDialog}
              />
            </div>,
            document.body,
          )
        : null}
    </>
  )
}

function ProviderMenuItem({
  danger = false,
  disabled = false,
  icon,
  label,
  onClick,
  title,
}: {
  danger?: boolean
  disabled?: boolean
  icon: typeof AtIcon
  label: string
  onClick: () => void
  title?: string
}) {
  return (
    <button
      type="button"
      role="menuitem"
      className={`provider-actions-item${danger ? ' provider-actions-item--danger' : ''}`}
      disabled={disabled}
      title={title}
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

function TableMessage({ children }: { children: string }) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={6}>
        {children}
      </td>
    </tr>
  )
}

function TableError({
  message,
  onRetry,
}: {
  message: string
  onRetry: () => void
}) {
  return (
    <tr>
      <td className="providers-table-message" colSpan={6} title={message}>
        <span>Couldn&apos;t load providers</span>
        <button type="button" className="providers-retry-button" onClick={onRetry}>
          Retry
        </button>
      </td>
    </tr>
  )
}

function formatCreatedAt(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : DATE_FORMATTER.format(date)
}
