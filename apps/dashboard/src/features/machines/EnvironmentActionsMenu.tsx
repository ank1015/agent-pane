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
  useRef,
  useState,
} from 'react'
import { createPortal } from 'react-dom'
import type { MachineEnvironment } from './machine-queries'

const MENU_WIDTH = 208
const MENU_ESTIMATED_HEIGHT = 78
const MENU_VIEWPORT_GAP = 8
const MENU_TRIGGER_GAP = 4

type EnvironmentActionsMenuProps = {
  environment: MachineEnvironment
  onDelete: () => void
  onUpdateName: () => void
}

export function EnvironmentActionsMenu({
  environment,
  onDelete,
  onUpdateName,
}: EnvironmentActionsMenuProps) {
  const [isOpen, setIsOpen] = useState(false)
  const [position, setPosition] = useState({ left: 0, top: 0 })
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const menuId = useId()

  const positionMenu = useCallback((menuHeight = MENU_ESTIMATED_HEIGHT) => {
    const trigger = triggerRef.current
    if (trigger === null) {
      return
    }

    const bounds = trigger.getBoundingClientRect()
    const left = Math.min(
      Math.max(MENU_VIEWPORT_GAP, bounds.right - MENU_WIDTH),
      window.innerWidth - MENU_WIDTH - MENU_VIEWPORT_GAP,
    )
    const hasRoomBelow =
      window.innerHeight - bounds.bottom >=
      menuHeight + MENU_TRIGGER_GAP + MENU_VIEWPORT_GAP
    const top = hasRoomBelow
      ? bounds.bottom + MENU_TRIGGER_GAP
      : Math.max(
          MENU_VIEWPORT_GAP,
          bounds.top - menuHeight - MENU_TRIGGER_GAP,
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

  const openDialog = (dialog: () => void) => {
    closeMenu()
    dialog()
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className="cursor-button cursor-button--ghost cursor-icon-button provider-actions-trigger"
        aria-label={`Actions for ${environment.name}`}
        aria-haspopup="menu"
        aria-expanded={isOpen}
        aria-controls={isOpen ? menuId : undefined}
        onClick={() => {
          if (!isOpen) {
            positionMenu()
          }
          setIsOpen((open) => !open)
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
              className="provider-actions-menu"
              role="menu"
              aria-label={`Actions for ${environment.name}`}
              style={position}
            >
              <EnvironmentMenuItem
                icon={AtIcon}
                label="Update name"
                onClick={() => openDialog(onUpdateName)}
              />
              <div className="provider-actions-separator" role="separator" />
              <EnvironmentMenuItem
                icon={Delete03Icon}
                label="Delete"
                danger
                onClick={() => openDialog(onDelete)}
              />
            </div>,
            document.body,
          )
        : null}
    </>
  )
}

function EnvironmentMenuItem({
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
