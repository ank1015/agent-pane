import { useCallback, useId, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import {
  type SandboxProvider,
  useCreateSandbox,
} from './machine-queries'

type AddSandboxStep = 'name' | 'template'

type AddSandboxDialogProps = {
  accountId: string
  open: boolean
  provider: SandboxProvider
  onClose: () => void
}

export function AddSandboxDialog({
  accountId,
  open,
  provider,
  onClose,
}: AddSandboxDialogProps) {
  const [step, setStep] = useState<AddSandboxStep>('name')
  const [name, setName] = useState('')
  const [templateId, setTemplateId] = useState('')
  const nameId = useId()
  const templateIdId = useId()
  const createSandbox = useCreateSandbox()
  const resetCreateSandbox = createSandbox.reset
  const usesTemplate = provider === 'e2b'

  const closeDialog = useCallback(() => {
    resetCreateSandbox()
    setStep('name')
    setName('')
    setTemplateId('')
    onClose()
  }, [onClose, resetCreateSandbox])

  const finish = async () => {
    const normalizedName = name.trim()
    if (createSandbox.isPending || normalizedName.length === 0) {
      return
    }

    const normalizedTemplateId = templateId.trim()
    try {
      await createSandbox.mutateAsync({
        accountId,
        name: normalizedName,
        templateId:
          usesTemplate && normalizedTemplateId.length > 0
            ? normalizedTemplateId
            : undefined,
      })
      closeDialog()
    } catch {
      // The mutation error is rendered on the current step.
    }
  }

  const runPrimaryAction = () => {
    if (step === 'name' && usesTemplate) {
      if (name.trim().length > 0) {
        setStep('template')
      }
      return
    }
    void finish()
  }

  const isFinishing = !usesTemplate || step === 'template'

  return (
    <Dialog
      open={open}
      title="Add Sandbox"
      dismissible={!createSandbox.isPending}
      onClose={closeDialog}
      footer={
        <>
          {step === 'template' ? (
            <button
              type="button"
              className="cursor-button cursor-button--ghost add-environment-back-button"
              disabled={createSandbox.isPending}
              onClick={() => {
                createSandbox.reset()
                setStep('name')
              }}
            >
              Back
            </button>
          ) : null}
          <button
            type="button"
            className="cursor-button add-environment-next-button"
            disabled={createSandbox.isPending || name.trim().length === 0}
            onClick={runPrimaryAction}
          >
            {isFinishing
              ? createSandbox.isPending
                ? 'Creating...'
                : 'Finish'
              : 'Next'}
          </button>
        </>
      }
    >
      <div className="add-environment-step">
        {step === 'name' ? (
          <div className="add-environment-field">
            <label htmlFor={nameId}>Name</label>
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
                  if (usesTemplate) {
                    setStep('template')
                  } else {
                    void finish()
                  }
                }
              }}
            />
            {!usesTemplate && createSandbox.isError ? (
              <p className="provider-create-error">
                {createSandbox.error.message}
              </p>
            ) : null}
          </div>
        ) : (
          <div className="add-environment-field">
            <label htmlFor={templateIdId}>Template ID</label>
            <input
              id={templateIdId}
              autoFocus
              type="text"
              className="cursor-input"
              value={templateId}
              placeholder="Defaults to base"
              autoComplete="off"
              onChange={(event) => setTemplateId(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') {
                  event.preventDefault()
                  void finish()
                }
              }}
            />
            {createSandbox.isError ? (
              <p className="provider-create-error">
                {createSandbox.error.message}
              </p>
            ) : null}
          </div>
        )}
      </div>
    </Dialog>
  )
}
