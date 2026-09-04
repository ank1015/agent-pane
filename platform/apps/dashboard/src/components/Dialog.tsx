import { useEffect, useId, useRef, type ReactNode, type RefObject } from 'react'

type DialogProps = {
  className?: string
  children: ReactNode
  dismissible?: boolean
  footer: ReactNode
  initialFocusRef?: RefObject<HTMLElement | null>
  title: string
  onClose: () => void
}

export function Dialog({ children, className = '', dismissible = true, footer, initialFocusRef, title, onClose }: DialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null)
  const titleId = useId()

  useEffect(() => {
    const dialog = dialogRef.current!
    dialog.showModal()
    initialFocusRef?.current?.focus()
    return () => dialog.close()
  }, [initialFocusRef])

  return (
    <dialog
      ref={dialogRef}
      className={`app-dialog ${className}`}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault()
        if (dismissible) onClose()
      }}
      onClick={(event) => {
        if (dismissible && event.target === event.currentTarget) onClose()
      }}
    >
      <div className="app-dialog-panel">
        <header className="app-dialog-header">
          <h2 id={titleId} className="cursor-page-title app-dialog-title">{title}</h2>
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
        <footer className="app-dialog-footer">{footer}</footer>
      </div>
    </dialog>
  )
}
