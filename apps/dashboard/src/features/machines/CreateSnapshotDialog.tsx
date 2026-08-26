import { useCallback, useId, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import { useCreateSandboxSnapshot } from './machine-queries'

type CreateSnapshotDialogProps = {
  accountId: string
  open: boolean
  sandboxId: string
  onClose: () => void
}

export function CreateSnapshotDialog({
  accountId,
  open,
  sandboxId,
  onClose,
}: CreateSnapshotDialogProps) {
  const [name, setName] = useState('')
  const nameId = useId()
  const createSnapshot = useCreateSandboxSnapshot()
  const resetCreateSnapshot = createSnapshot.reset

  const closeDialog = useCallback(() => {
    resetCreateSnapshot()
    setName('')
    onClose()
  }, [onClose, resetCreateSnapshot])

  const finish = async () => {
    const normalizedName = name.trim()
    if (createSnapshot.isPending || normalizedName.length === 0) {
      return
    }

    try {
      await createSnapshot.mutateAsync({
        accountId,
        sandboxId,
        name: normalizedName,
      })
      closeDialog()
    } catch {
      // The mutation error is rendered below the input.
    }
  }

  return (
    <Dialog
      className="create-snapshot-dialog"
      open={open}
      title="Create Snapshot"
      dismissible={!createSnapshot.isPending}
      onClose={closeDialog}
      footer={
        <button
          type="button"
          className="cursor-button add-environment-next-button"
          disabled={createSnapshot.isPending || name.trim().length === 0}
          onClick={() => void finish()}
        >
          {createSnapshot.isPending ? 'Creating...' : 'Finish'}
        </button>
      }
    >
      <div className="add-environment-step">
        <div className="add-environment-field">
          <label htmlFor={nameId}>Name</label>
          <input
            id={nameId}
            autoFocus
            type="text"
            className="cursor-input"
            value={name}
            placeholder="e.g. Ready workspace"
            autoComplete="off"
            maxLength={120}
            required
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && name.trim().length > 0) {
                event.preventDefault()
                void finish()
              }
            }}
          />
          {createSnapshot.isError ? (
            <p className="provider-create-error">
              {createSnapshot.error.message}
            </p>
          ) : null}
        </div>
      </div>
    </Dialog>
  )
}
