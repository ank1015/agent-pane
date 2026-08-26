import { MoreHorizontalIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type { IconSvgElement } from '@hugeicons/react'
import {
  Fragment,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import { createPortal } from 'react-dom'

const MENU_WIDTH = 208
const MENU_ITEM_ESTIMATED_HEIGHT = 36
const MENU_VIEWPORT_GAP = 8
const MENU_TRIGGER_GAP = 4

export type TableAction = {
  danger?: boolean
  icon: IconSvgElement
  label: string
  separatorBefore?: boolean
  onSelect?: () => void
}

export function TableActionsMenu({
  actions,
  label,
}: {
  actions: TableAction[]
  label: string
}) {
  const [isOpen, setIsOpen] = useState(false)
  const [position, setPosition] = useState({ left: 0, top: 0 })
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const menuId = useId()
  const hasActions = actions.length > 0

  const positionMenu = useCallback(
    (menuHeight = actions.length * MENU_ITEM_ESTIMATED_HEIGHT + 8) => {
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
    },
    [actions.length],
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

  const selectAction = (action: TableAction) => {
    closeMenu()
    action.onSelect?.()
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className="cursor-button cursor-button--ghost cursor-icon-button provider-actions-trigger"
        aria-label={label}
        aria-haspopup={hasActions ? 'menu' : undefined}
        aria-expanded={hasActions ? isOpen : undefined}
        aria-controls={isOpen ? menuId : undefined}
        disabled={!hasActions}
        onClick={() => {
          if (!hasActions) {
            return
          }
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
              aria-label={label}
              style={position}
            >
              {actions.map((action) => (
                <Fragment key={action.label}>
                  {action.separatorBefore ? (
                    <div
                      className="provider-actions-separator"
                      role="separator"
                    />
                  ) : null}
                  <button
                    type="button"
                    role="menuitem"
                    className={`provider-actions-item${action.danger ? ' provider-actions-item--danger' : ''}`}
                    onClick={() => selectAction(action)}
                  >
                    <HugeiconsIcon
                      icon={action.icon}
                      size={16}
                      color="currentColor"
                      strokeWidth={1.5}
                      aria-hidden="true"
                    />
                    <span>{action.label}</span>
                  </button>
                </Fragment>
              ))}
            </div>,
            document.body,
          )
        : null}
    </>
  )
}
