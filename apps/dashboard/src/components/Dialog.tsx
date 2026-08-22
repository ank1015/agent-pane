import { useEffect, useId, useRef } from 'react'
import type { ReactNode } from 'react'

type DialogProps = {
  children: ReactNode
  className?: string
  dismissible?: boolean
  footer?: ReactNode
  open: boolean
  title: string
  onClose: () => void
}

export function Dialog({
  children,
  className,
  dismissible = true,
  footer,
  open,
  title,
  onClose,
}: DialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null)
  const titleId = useId()

  useEffect(() => {
    const dialog = dialogRef.current
    if (dialog === null) {
      return
    }

    if (open && !dialog.open) {
      dialog.showModal()
    } else if (!open && dialog.open) {
      dialog.close()
    }
  }, [open])

  useEffect(() => {
    if (!open || !dismissible) {
      return
    }

    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        onClose()
      }
    }

    document.addEventListener('keydown', closeOnEscape)
    return () => document.removeEventListener('keydown', closeOnEscape)
  }, [dismissible, onClose, open])

  return (
    <dialog
      ref={dialogRef}
      className={`app-dialog${className ? ` ${className}` : ''}`}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault()
        if (dismissible) {
          onClose()
        }
      }}
      onClick={(event) => {
        if (dismissible && event.target === event.currentTarget) {
          onClose()
        }
      }}
    >
      <div className="app-dialog-panel">
        <header className="app-dialog-header">
          <h2 id={titleId} className="cursor-page-title app-dialog-title">
            {title}
          </h2>
          <button
            type="button"
            className="cursor-button cursor-button--ghost cursor-icon-button app-dialog-close"
            aria-label="Close dialog"
            disabled={!dismissible}
            onClick={onClose}
          >
            <span aria-hidden="true">×</span>
          </button>
        </header>

        <div className="app-dialog-body">{children}</div>

        {footer ? <footer className="app-dialog-footer">{footer}</footer> : null}
      </div>
    </dialog>
  )
}
