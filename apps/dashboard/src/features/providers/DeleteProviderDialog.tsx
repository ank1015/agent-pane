import { useEffect, useRef } from 'react'
import { Dialog } from '../../components/Dialog'
import { PROVIDER_LABELS } from './provider-metadata'
import {
  type ProviderAccount,
  useDeleteProviderAccount,
} from './provider-queries'

type DeleteProviderDialogProps = {
  account: ProviderAccount | null
  onClose: () => void
}

export function DeleteProviderDialog({
  account,
  onClose,
}: DeleteProviderDialogProps) {
  const deleteProviderAccount = useDeleteProviderAccount()
  const cancelButtonRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (account === null) {
      return
    }
    const frame = window.requestAnimationFrame(() => cancelButtonRef.current?.focus())
    return () => window.cancelAnimationFrame(frame)
  }, [account])

  const closeDialog = () => {
    if (deleteProviderAccount.isPending) {
      return
    }
    deleteProviderAccount.reset()
    onClose()
  }

  const confirmDelete = async () => {
    if (account === null) {
      return
    }

    try {
      await deleteProviderAccount.mutateAsync(account.id)
      deleteProviderAccount.reset()
      onClose()
    } catch {
      // The mutation state renders the platform or gateway error below.
    }
  }

  return (
    <Dialog
      className="delete-provider-dialog"
      dismissible={!deleteProviderAccount.isPending}
      open={account !== null}
      title={account === null ? 'Delete provider' : `Delete ${account.name}`}
      onClose={closeDialog}
      footer={
        <>
          <button
            ref={cancelButtonRef}
            type="button"
            className="cursor-button cursor-button--ghost"
            disabled={deleteProviderAccount.isPending}
            onClick={closeDialog}
          >
            Cancel
          </button>
          <button
            type="button"
            className="cursor-button delete-provider-confirm-button"
            disabled={deleteProviderAccount.isPending}
            onClick={() => void confirmDelete()}
          >
            {deleteProviderAccount.isPending ? 'Deleting…' : 'Delete'}
          </button>
        </>
      }
    >
      {account !== null ? (
        <div className="delete-provider-content">
          <p className="delete-provider-description">
            This permanently removes the {PROVIDER_LABELS[account.provider]} account
            and its stored credentials. This action cannot be undone.
          </p>
          {deleteProviderAccount.isError ? (
            <p className="delete-provider-error" role="alert">
              {deleteProviderAccount.error.message}
            </p>
          ) : null}
        </div>
      ) : null}
    </Dialog>
  )
}
