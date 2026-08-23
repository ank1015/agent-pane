import { useEffect, useRef } from 'react'
import { Dialog } from '../../components/Dialog'
import { type MachineTarget, useDeleteMachine } from './machine-queries'

const SANDBOX_LABELS = {
  blaxel: 'Blaxel',
  daytona: 'Daytona',
  e2b: 'E2B',
  tensorlake: 'Tensorlake',
} as const

type DeleteMachineDialogProps = {
  target: MachineTarget | null
  onClose: () => void
}

export function DeleteMachineDialog({
  target,
  onClose,
}: DeleteMachineDialogProps) {
  const deleteMachine = useDeleteMachine()
  const cancelButtonRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (target === null) {
      return
    }
    const frame = window.requestAnimationFrame(() =>
      cancelButtonRef.current?.focus(),
    )
    return () => window.cancelAnimationFrame(frame)
  }, [target])

  const closeDialog = () => {
    if (deleteMachine.isPending) {
      return
    }
    deleteMachine.reset()
    onClose()
  }

  const confirmDelete = async () => {
    if (target === null) {
      return
    }
    try {
      await deleteMachine.mutateAsync(target)
      deleteMachine.reset()
      onClose()
    } catch {
      // The mutation error is displayed in the dialog.
    }
  }

  const name =
    target === null
      ? ''
      : target.kind === 'tunnel'
        ? target.machine.name
        : target.account.name

  return (
    <Dialog
      className="delete-provider-dialog"
      dismissible={!deleteMachine.isPending}
      open={target !== null}
      title={target === null ? 'Delete' : `Delete ${name}`}
      onClose={closeDialog}
      footer={
        <>
          <button
            ref={cancelButtonRef}
            type="button"
            className="cursor-button cursor-button--ghost"
            disabled={deleteMachine.isPending}
            onClick={closeDialog}
          >
            Cancel
          </button>
          <button
            type="button"
            className="cursor-button delete-provider-confirm-button"
            disabled={deleteMachine.isPending}
            onClick={() => void confirmDelete()}
          >
            {deleteMachine.isPending ? 'Deleting…' : 'Delete'}
          </button>
        </>
      }
    >
      {target !== null ? (
        <div className="delete-provider-content">
          <p className="delete-provider-description">
            {target.kind === 'tunnel'
              ? 'This removes the machine tunnel and revokes its credential. The machine must be registered again to reconnect. This action cannot be undone.'
              : `This permanently removes the ${SANDBOX_LABELS[target.account.provider]} account and its stored credentials. This action cannot be undone.`}
          </p>
          {deleteMachine.isError ? (
            <p className="delete-provider-error" role="alert">
              {deleteMachine.error.message}
            </p>
          ) : null}
        </div>
      ) : null}
    </Dialog>
  )
}
