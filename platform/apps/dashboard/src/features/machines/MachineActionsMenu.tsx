import { AtIcon, Delete03Icon, MoreHorizontalIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'

export function MachineActionsMenu({ name, onAction }: { name: string; onAction: (action: 'rename' | 'delete', trigger: HTMLButtonElement) => void }) {
  const [open, setOpen] = useState(false)
  const [position, setPosition] = useState({ left: 0, top: 0 })
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const initialItem = useRef(0)
  const menuId = useId()
  const close = useCallback((restoreFocus = false) => {
    setOpen(false)
    if (restoreFocus) trigger.current?.focus()
  }, [])

  useLayoutEffect(() => {
    if (!open || !trigger.current || !menu.current) return
    const bounds = trigger.current.getBoundingClientRect()
    const height = menu.current.offsetHeight
    setPosition({
      left: Math.max(8, Math.min(bounds.right - 208, window.innerWidth - 216)),
      top: window.innerHeight - bounds.bottom >= height + 12 ? bounds.bottom + 4 : Math.max(8, bounds.top - height - 4),
    })
    menu.current.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')[initialItem.current]?.focus()
  }, [open])

  useEffect(() => {
    if (!open) return
    const outside = (event: PointerEvent) => {
      if (event.target instanceof Node && !menu.current?.contains(event.target) && !trigger.current?.contains(event.target)) close()
    }
    const viewport = () => close()
    document.addEventListener('pointerdown', outside)
    window.addEventListener('resize', viewport)
    window.addEventListener('scroll', viewport, true)
    return () => {
      document.removeEventListener('pointerdown', outside)
      window.removeEventListener('resize', viewport)
      window.removeEventListener('scroll', viewport, true)
    }
  }, [open, close])

  const choose = (action: 'rename' | 'delete') => {
    close()
    if (trigger.current) onAction(action, trigger.current)
  }
  return <>
    <button ref={trigger} type="button" className="cursor-button cursor-button--ghost cursor-icon-button machine-card-actions"
      aria-label={`Actions for ${name}`} aria-haspopup="menu" aria-expanded={open} aria-controls={open ? menuId : undefined}
      onClick={() => { initialItem.current = 0; setOpen((value) => !value) }}
      onKeyDown={(event) => { if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); initialItem.current = event.key === 'ArrowUp' ? 1 : 0; setOpen(true) } }}>
      <HugeiconsIcon icon={MoreHorizontalIcon} size={16} color="currentColor" strokeWidth={1.5} aria-hidden="true" />
    </button>
    {open ? createPortal(<div ref={menu} id={menuId} role="menu" aria-label={`Actions for ${name}`} className="provider-actions-menu machine-card-actions-menu" style={position}
      onKeyDown={(event) => {
        const items = Array.from(menu.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? [])
        const index = items.indexOf(document.activeElement as HTMLButtonElement)
        if (event.key === 'Escape') { event.preventDefault(); close(true) }
        if (event.key === 'Tab') { close(true) }
        if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
          event.preventDefault()
          const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length
          items[next]?.focus()
        }
      }}>
      <button type="button" role="menuitem" className="provider-actions-item" onClick={() => choose('rename')}><HugeiconsIcon icon={AtIcon} size={16} strokeWidth={1.5} aria-hidden="true" />Update Name</button>
      <div className="provider-actions-separator" role="separator" />
      <button type="button" role="menuitem" className="provider-actions-item provider-actions-item--danger" onClick={() => choose('delete')}><HugeiconsIcon icon={Delete03Icon} size={16} strokeWidth={1.5} aria-hidden="true" />Delete</button>
    </div>, document.body) : null}
  </>
}
