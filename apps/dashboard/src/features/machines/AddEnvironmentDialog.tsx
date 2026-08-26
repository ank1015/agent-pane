import { useCallback, useId, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import { useCreateMachineEnvironment } from './machine-queries'

type AddEnvironmentStep = 'name' | 'path'

type AddEnvironmentDialogProps = {
  machineId: string
  open: boolean
  workspaceRootId: string
  workspaceRootPath: string
  onClose: () => void
}

export function AddEnvironmentDialog({
  machineId,
  open,
  workspaceRootId,
  workspaceRootPath,
  onClose,
}: AddEnvironmentDialogProps) {
  const [step, setStep] = useState<AddEnvironmentStep>('name')
  const [name, setName] = useState('')
  const [path, setPath] = useState('')
  const nameId = useId()
  const pathId = useId()
  const pathPrefixId = useId()
  const createEnvironment = useCreateMachineEnvironment()

  const closeDialog = useCallback(() => {
    createEnvironment.reset()
    setStep('name')
    setName('')
    setPath('')
    onClose()
  }, [createEnvironment, onClose])

  const finish = async () => {
    if (
      createEnvironment.isPending ||
      name.trim().length === 0 ||
      path.trim().length === 0 ||
      workspaceRootId.length === 0
    ) {
      return
    }

    try {
      await createEnvironment.mutateAsync({
        machineId,
        name: name.trim(),
        workspaceRootId,
        path: path.trim(),
      })
      closeDialog()
    } catch {
      // The mutation error is rendered on the path step.
    }
  }

  const runPrimaryAction = () => {
    if (step === 'name') {
      if (name.trim().length > 0) {
        setStep('path')
      }
      return
    }

    void finish()
  }

  return (
    <Dialog
      open={open}
      title="Add Environment"
      dismissible={!createEnvironment.isPending}
      onClose={closeDialog}
      footer={
        <>
          {step === 'path' ? (
            <button
              type="button"
              className="cursor-button cursor-button--ghost add-environment-back-button"
              disabled={createEnvironment.isPending}
              onClick={() => {
                createEnvironment.reset()
                setStep('name')
              }}
            >
              Back
            </button>
          ) : null}
          <button
            type="button"
            className="cursor-button add-environment-next-button"
            disabled={
              createEnvironment.isPending ||
              (step === 'name'
                ? name.trim().length === 0
                : path.trim().length === 0 || workspaceRootId.length === 0)
            }
            onClick={runPrimaryAction}
          >
            {step === 'name'
              ? 'Next'
              : createEnvironment.isPending
                ? 'Creating...'
                : 'Finish'}
          </button>
        </>
      }
    >
      <div className="add-environment-step">
        {step === 'name' ? (
          <div className="add-environment-field">
            <label htmlFor={nameId}>Environment name</label>
            <input
              id={nameId}
              autoFocus
              type="text"
              className="cursor-input"
              value={name}
              placeholder="e.g. Development"
              autoComplete="off"
              required
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && name.trim().length > 0) {
                  event.preventDefault()
                  setStep('path')
                }
              }}
            />
          </div>
        ) : (
          <div className="add-environment-field">
            <label htmlFor={pathId}>Path</label>
            <div className="add-environment-path-input">
              <span
                id={pathPrefixId}
                className="add-environment-path-prefix"
                title={workspaceRootPath}
              >
                {workspaceRootPath}
              </span>
              <input
                id={pathId}
                autoFocus
                type="text"
                className="cursor-input"
                value={path}
                placeholder="relative/path"
                aria-describedby={pathPrefixId}
                autoComplete="off"
                required
                onChange={(event) => setPath(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' && path.trim().length > 0) {
                    event.preventDefault()
                    void finish()
                  }
                }}
              />
            </div>
            {createEnvironment.isError ? (
              <p className="provider-create-error">
                {createEnvironment.error.message}
              </p>
            ) : null}
          </div>
        )}
      </div>
    </Dialog>
  )
}
