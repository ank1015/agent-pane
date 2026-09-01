import { useEffect, useId, useRef, useState } from 'react'
import type { FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import {
  type ProjectEnvironment,
  useDeleteProjectEnvironment,
  useUpdateProjectEnvironmentName,
} from './project-queries'

type ProjectEnvironmentDialogProps = {
  projectId: string
  environment: ProjectEnvironment
  onClose: () => void
}

export function UpdateProjectEnvironmentNameDialog({
  projectId,
  environment,
  onClose,
}: ProjectEnvironmentDialogProps) {
  const [name, setName] = useState(() => environment.name)
  const updateName = useUpdateProjectEnvironmentName(projectId)
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
    normalizedName !== environment.name &&
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
      await updateName.mutateAsync({ environment, name: normalizedName })
      updateName.reset()
      onClose()
    } catch {
      // The mutation error is rendered below the field.
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
          <label htmlFor={inputId}>Environment name</label>
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

export function DeleteProjectEnvironmentDialog({
  projectId,
  environment,
  onClose,
}: ProjectEnvironmentDialogProps) {
  const deleteEnvironment = useDeleteProjectEnvironment(projectId)
  const cancelButtonRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    const frame = window.requestAnimationFrame(() =>
      cancelButtonRef.current?.focus(),
    )
    return () => window.cancelAnimationFrame(frame)
  }, [])

  const closeDialog = () => {
    if (deleteEnvironment.isPending) {
      return
    }
    deleteEnvironment.reset()
    onClose()
  }

  const confirmDelete = async () => {
    try {
      await deleteEnvironment.mutateAsync(environment)
      deleteEnvironment.reset()
      onClose()
    } catch {
      // The mutation error is rendered in the dialog.
    }
  }

  return (
    <Dialog
      className="delete-provider-dialog"
      dismissible={!deleteEnvironment.isPending}
      open
      title={`Delete ${environment.name}`}
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
    </Dialog>
  )
}
