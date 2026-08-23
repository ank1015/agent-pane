import { useEffect, useId, useRef, useState } from 'react'
import type { FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import {
  type MachineTarget,
  useUpdateMachineName,
} from './machine-queries'

type UpdateMachineNameDialogProps = {
  target: MachineTarget
  onClose: () => void
}

export function UpdateMachineNameDialog({
  target,
  onClose,
}: UpdateMachineNameDialogProps) {
  const currentName = target.kind === 'tunnel' ? target.machine.name : target.account.name
  const [name, setName] = useState(currentName)
  const updateName = useUpdateMachineName()
  const inputRef = useRef<HTMLInputElement>(null)
  const formId = useId()
  const inputId = useId()
  const errorId = useId()

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      inputRef.current?.focus()
      inputRef.current?.select()
    })
    return () => window.cancelAnimationFrame(frame)
  }, [])

  const normalizedName = name.trim()
  const canConfirm =
    normalizedName.length > 0 &&
    normalizedName !== currentName &&
    !updateName.isPending

  const closeDialog = () => {
    if (updateName.isPending) {
      return
    }
    updateName.reset()
    onClose()
  }

  const confirmUpdate = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!canConfirm) {
      return
    }

    try {
      await updateName.mutateAsync({ target, name: normalizedName })
      updateName.reset()
      onClose()
    } catch {
      // The mutation error is displayed below the field.
    }
  }

  return (
    <Dialog
      className="update-provider-name-dialog"
      dismissible={!updateName.isPending}
      open
      title="Update name"
      onClose={closeDialog}
      footer={
        <>
          <button
            type="button"
            className="cursor-button cursor-button--ghost"
            disabled={updateName.isPending}
            onClick={closeDialog}
          >
            Cancel
          </button>
          <button
            type="submit"
            form={formId}
            className="cursor-button"
            disabled={!canConfirm}
          >
            {updateName.isPending ? 'Confirming…' : 'Confirm'}
          </button>
        </>
      }
    >
      <form id={formId} onSubmit={(event) => void confirmUpdate(event)}>
        <div className="update-provider-name-field">
          <label htmlFor={inputId}>
            {target.kind === 'tunnel' ? 'Machine name' : 'Account name'}
          </label>
          <input
            ref={inputRef}
            id={inputId}
            type="text"
            className="cursor-input"
            value={name}
            autoComplete="off"
            disabled={updateName.isPending}
            required
            maxLength={120}
            aria-invalid={updateName.isError}
            aria-describedby={updateName.isError ? errorId : undefined}
            onChange={(event) => {
              setName(event.target.value)
              updateName.reset()
            }}
          />
        </div>
        {updateName.isError ? (
          <p id={errorId} className="update-provider-name-error" role="alert">
            {updateName.error.message}
          </p>
        ) : null}
      </form>
    </Dialog>
  )
}
