import { useCallback, useId, useState } from 'react'
import { Dialog } from '../../components/Dialog'

type EnvironmentDialogStep = 'name' | 'path'

export type EnvironmentDraft = {
  name: string
  path: string
}

type EnvironmentDialogFlowProps = {
  canFinish?: boolean
  error?: string | null
  isPending?: boolean
  open: boolean
  workspaceRootPath: string
  onClose: () => void
  onFinish: (draft: EnvironmentDraft) => Promise<unknown> | unknown
  onReset?: () => void
}

export function EnvironmentDialogFlow({
  canFinish = true,
  error = null,
  isPending = false,
  open,
  workspaceRootPath,
  onClose,
  onFinish,
  onReset,
}: EnvironmentDialogFlowProps) {
  const [step, setStep] = useState<EnvironmentDialogStep>('name')
  const [name, setName] = useState('')
  const [path, setPath] = useState('')
  const nameId = useId()
  const pathId = useId()
  const pathPrefixId = useId()

  const closeDialog = useCallback(() => {
    onReset?.()
    setStep('name')
    setName('')
    setPath('')
    onClose()
  }, [onClose, onReset])

  const finish = async () => {
    const normalizedName = name.trim()
    const normalizedPath = path.trim()
    if (
      isPending ||
      !canFinish ||
      normalizedName.length === 0 ||
      normalizedPath.length === 0
    ) {
      return
    }

    try {
      await onFinish({ name: normalizedName, path: normalizedPath })
      closeDialog()
    } catch {
      // The parent-provided error is rendered on the path step.
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
      dismissible={!isPending}
      onClose={closeDialog}
      footer={
        <>
          {step === 'path' ? (
            <button
              type="button"
              className="cursor-button cursor-button--ghost add-environment-back-button"
              disabled={isPending}
              onClick={() => {
                onReset?.()
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
              isPending ||
              (step === 'name'
                ? name.trim().length === 0
                : path.trim().length === 0 || !canFinish)
            }
            onClick={runPrimaryAction}
          >
            {step === 'name'
              ? 'Next'
              : isPending
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
            {error !== null ? (
              <p className="provider-create-error">{error}</p>
            ) : null}
          </div>
        )}
      </div>
    </Dialog>
  )
}
