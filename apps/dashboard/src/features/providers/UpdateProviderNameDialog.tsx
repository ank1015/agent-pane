import { useEffect, useId, useRef, useState } from 'react'
import type { FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import {
  type ProviderAccount,
  useUpdateProviderAccountName,
} from './provider-queries'

type UpdateProviderNameDialogProps = {
  account: ProviderAccount
  onClose: () => void
}

export function UpdateProviderNameDialog({
  account,
  onClose,
}: UpdateProviderNameDialogProps) {
  const [name, setName] = useState(() => account.name)
  const updateProviderName = useUpdateProviderAccountName()
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
    normalizedName !== account.name &&
    !updateProviderName.isPending

  const closeDialog = () => {
    if (updateProviderName.isPending) {
      return
    }
    updateProviderName.reset()
    onClose()
  }

  const confirmUpdate = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!canConfirm) {
      return
    }

    try {
      await updateProviderName.mutateAsync({
        providerId: account.id,
        name: normalizedName,
      })
      updateProviderName.reset()
      onClose()
    } catch {
      // The mutation state renders the platform or gateway error below.
    }
  }

  return (
    <Dialog
      className="update-provider-name-dialog"
      dismissible={!updateProviderName.isPending}
      open
      title="Update name"
      onClose={closeDialog}
      footer={
        <>
          <button
            type="button"
            className="cursor-button cursor-button--ghost"
            disabled={updateProviderName.isPending}
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
            {updateProviderName.isPending ? 'Confirming…' : 'Confirm'}
          </button>
        </>
      }
    >
      <form id={formId} onSubmit={(event) => void confirmUpdate(event)}>
        <div className="update-provider-name-field">
          <label htmlFor={inputId}>Account name</label>
          <input
            ref={inputRef}
            id={inputId}
            type="text"
            className="cursor-input"
            value={name}
            autoComplete="off"
            disabled={updateProviderName.isPending}
            required
            aria-invalid={updateProviderName.isError}
            aria-describedby={updateProviderName.isError ? errorId : undefined}
            onChange={(event) => {
              setName(event.target.value)
              updateProviderName.reset()
            }}
          />
        </div>
        {updateProviderName.isError ? (
          <p id={errorId} className="update-provider-name-error" role="alert">
            {updateProviderName.error.message}
          </p>
        ) : null}
      </form>
    </Dialog>
  )
}
