import { useCallback, useId, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import { useCreateE2bSandbox } from './machine-queries'

type AddSandboxStep = 'name' | 'template'

type AddE2bSandboxDialogProps = {
  accountId: string
  open: boolean
  onClose: () => void
}

export function AddE2bSandboxDialog({
  accountId,
  open,
  onClose,
}: AddE2bSandboxDialogProps) {
  const [step, setStep] = useState<AddSandboxStep>('name')
  const [name, setName] = useState('')
  const [templateId, setTemplateId] = useState('')
  const nameId = useId()
  const templateIdId = useId()
  const createSandbox = useCreateE2bSandbox()

  const closeDialog = useCallback(() => {
    createSandbox.reset()
    setStep('name')
    setName('')
    setTemplateId('')
    onClose()
  }, [createSandbox, onClose])

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
          normalizedTemplateId.length === 0
            ? undefined
            : normalizedTemplateId,
      })
      closeDialog()
    } catch {
      // The mutation error is rendered on the template step.
    }
  }

  const runPrimaryAction = () => {
    if (step === 'name') {
      if (name.trim().length > 0) {
        setStep('template')
      }
      return
    }

    void finish()
  }

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
            {step === 'name'
              ? 'Next'
              : createSandbox.isPending
                ? 'Creating...'
                : 'Finish'}
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
                  setStep('template')
                }
              }}
            />
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
