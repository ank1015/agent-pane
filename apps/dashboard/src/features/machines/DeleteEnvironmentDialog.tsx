import { useEffect, useRef } from 'react'
import { Dialog } from '../../components/Dialog'
import {
  type MachineEnvironment,
  useDeleteMachineEnvironment,
} from './machine-queries'

type DeleteEnvironmentDialogProps = {
  environment: MachineEnvironment | null
  onClose: () => void
}

export function DeleteEnvironmentDialog({
  environment,
  onClose,
}: DeleteEnvironmentDialogProps) {
  const deleteEnvironment = useDeleteMachineEnvironment()
  const cancelButtonRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (environment === null) {
      return
    }
    const frame = window.requestAnimationFrame(() =>
      cancelButtonRef.current?.focus(),
    )
    return () => window.cancelAnimationFrame(frame)
  }, [environment])

  const closeDialog = () => {
    if (deleteEnvironment.isPending) {
      return
    }
    deleteEnvironment.reset()
    onClose()
  }

  const confirmDelete = async () => {
    if (environment === null) {
      return
    }

    try {
      await deleteEnvironment.mutateAsync(environment)
      deleteEnvironment.reset()
      onClose()
    } catch {
      // The mutation error is displayed in the dialog.
    }
  }

  return (
    <Dialog
      className="delete-provider-dialog"
      dismissible={!deleteEnvironment.isPending}
      open={environment !== null}
      title={
        environment === null
          ? 'Delete environment'
          : `Delete ${environment.name}`
      }
      onClose={closeDialog}
      footer={
        <>
          <button
            ref={cancelButtonRef}
            type="button"
            className="cursor-button cursor-button--ghost"
            disabled={deleteEnvironment.isPending}
            onClick={closeDialog}
          >
            Cancel
          </button>
          <button
            type="button"
            className="cursor-button delete-provider-confirm-button"
            disabled={deleteEnvironment.isPending}
            onClick={() => void confirmDelete()}
          >
            {deleteEnvironment.isPending ? 'Deleting…' : 'Delete'}
          </button>
        </>
      }
    >
      {environment !== null ? (
        <div className="delete-provider-content">
          <p className="delete-provider-description">
            This permanently removes the environment. This action cannot be
            undone.
          </p>
          {deleteEnvironment.isError ? (
            <p className="delete-provider-error" role="alert">
              {deleteEnvironment.error.message}
            </p>
          ) : null}
        </div>
      ) : null}
    </Dialog>
  )
}
