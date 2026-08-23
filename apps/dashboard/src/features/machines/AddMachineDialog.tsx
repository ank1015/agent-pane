import { ViewIcon, ViewOffIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useId, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import { SANDBOX_OPTIONS } from './machine-metadata'
import {
  type SandboxProvider,
  useCreateSandboxAccount,
} from './machine-queries'

type AddSandboxDialogProps = {
  open: boolean
  onClose: () => void
}

type AddMachineStep = 'connector' | 'account' | 'credentials'

export function AddSandboxDialog({ open, onClose }: AddSandboxDialogProps) {
  const [step, setStep] = useState<AddMachineStep>('connector')
  const [selectedConnector, setSelectedConnector] =
    useState<SandboxProvider | null>(null)
  const [accountName, setAccountName] = useState('')
  const [apiKey, setApiKey] = useState('')
  const createSandboxAccount = useCreateSandboxAccount()

  const closeDialog = useCallback(() => {
    setStep('connector')
    setSelectedConnector(null)
    setAccountName('')
    setApiKey('')
    createSandboxAccount.reset()
    onClose()
  }, [createSandboxAccount, onClose])

  const selectConnector = (connector: SandboxProvider) => {
    if (connector !== selectedConnector) {
      createSandboxAccount.reset()
      setAccountName('')
      setApiKey('')
    }
    setSelectedConnector(connector)
  }

  const goNext = () => {
    if (step === 'connector' && selectedConnector !== null) {
      setStep('account')
    } else if (step === 'account' && accountName.trim().length > 0) {
      setStep('credentials')
    }
  }

  const goBack = () => {
    createSandboxAccount.reset()
    setStep(step === 'credentials' ? 'account' : 'connector')
  }

  const isNextDisabled =
    step === 'connector'
      ? selectedConnector === null
      : step === 'account'
        ? accountName.trim().length === 0
        : apiKey.trim().length === 0

  const finish = async () => {
    if (step !== 'credentials' || selectedConnector === null) {
      return
    }

    try {
      await createSandboxAccount.mutateAsync({
        provider: selectedConnector,
        name: accountName.trim(),
        apiKey,
      })
      closeDialog()
    } catch {
      // The mutation state renders the platform or gateway error in the dialog.
    }
  }

  const runPrimaryAction = () => {
    if (step === 'credentials') {
      void finish()
    } else {
      goNext()
    }
  }

  return (
    <Dialog
      open={open}
      title="Add Sandbox"
      onClose={closeDialog}
      footer={
        <>
          {step !== 'connector' ? (
            <button
              type="button"
              className="cursor-button cursor-button--ghost add-provider-back-button"
              disabled={createSandboxAccount.isPending}
              onClick={goBack}
            >
              Back
            </button>
          ) : null}
          <button
            type="button"
            className="cursor-button add-provider-next-button"
            disabled={isNextDisabled || createSandboxAccount.isPending}
            onClick={runPrimaryAction}
          >
            {createSandboxAccount.isPending
              ? 'Adding...'
              : step === 'credentials'
                ? 'Finish'
                : 'Next'}
          </button>
        </>
      }
    >
      {step === 'connector' ? (
        <MachineSelectionStep
          selectedConnector={selectedConnector}
          onSelect={selectConnector}
        />
      ) : null}
      {step === 'account' ? (
        <MachineAccountNameStep
          accountName={accountName}
          onAccountNameChange={setAccountName}
        />
      ) : null}
      {step === 'credentials' && selectedConnector !== null ? (
        <MachineApiKeyStep
          connector={selectedConnector}
          apiKey={apiKey}
          error={
            createSandboxAccount.isError
              ? createSandboxAccount.error.message
              : null
          }
          onApiKeyChange={setApiKey}
        />
      ) : null}
    </Dialog>
  )
}

function MachineApiKeyStep({
  connector,
  apiKey,
  error,
  onApiKeyChange,
}: {
  connector: SandboxProvider
  apiKey: string
  error: string | null
  onApiKeyChange: (value: string) => void
}) {
  const apiKeyInputId = useId()
  const [isApiKeyVisible, setIsApiKeyVisible] = useState(false)
  const connectorLabel =
    SANDBOX_OPTIONS.find((option) => option.id === connector)?.label ?? connector

  return (
    <div className="provider-credentials-step">
      <div className="provider-api-key-field">
        <label htmlFor={apiKeyInputId}>{connectorLabel} API key</label>
        <div className="provider-api-key-input-wrap">
          <input
            id={apiKeyInputId}
            autoFocus
            type={isApiKeyVisible ? 'text' : 'password'}
            className="cursor-input"
            value={apiKey}
            placeholder="Enter API key"
            autoComplete="off"
            spellCheck={false}
            required
            onChange={(event) => onApiKeyChange(event.target.value)}
          />
          <button
            type="button"
            className="cursor-button cursor-button--ghost cursor-icon-button provider-api-key-visibility"
            aria-label={isApiKeyVisible ? 'Hide API key' : 'Show API key'}
            aria-pressed={isApiKeyVisible}
            onClick={() => setIsApiKeyVisible((isVisible) => !isVisible)}
          >
            <HugeiconsIcon
              icon={isApiKeyVisible ? ViewIcon : ViewOffIcon}
              size={17}
              color="currentColor"
              strokeWidth={1.5}
              aria-hidden="true"
            />
          </button>
        </div>
      </div>
      {error !== null ? (
        <p className="provider-create-error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  )
}

function MachineSelectionStep({
  selectedConnector,
  onSelect,
}: {
  selectedConnector: SandboxProvider | null
  onSelect: (connector: SandboxProvider) => void
}) {
  return (
    <>
      <div className="provider-selection-spacer" aria-hidden="true" />

      <fieldset className="provider-option-grid">
        <legend className="visually-hidden">Machine connection type</legend>
        {SANDBOX_OPTIONS.map((option) => (
          <label className="provider-option" key={option.id}>
            <input
              type="radio"
              name="machine-connector"
              value={option.id}
              checked={selectedConnector === option.id}
              onChange={() => onSelect(option.id)}
            />
            <span className="provider-option-card">
              <span className="provider-option-icon" aria-hidden="true">
                <HugeiconsIcon
                  icon={option.icon}
                  size={18}
                  color="currentColor"
                  strokeWidth={1.5}
                />
              </span>
              <span>{option.label}</span>
            </span>
          </label>
        ))}
      </fieldset>
    </>
  )
}

function MachineAccountNameStep({
  accountName,
  onAccountNameChange,
}: {
  accountName: string
  onAccountNameChange: (value: string) => void
}) {
  const accountNameId = useId()

  return (
    <div className="provider-account-step">
      <div className="provider-account-name-field">
        <label htmlFor={accountNameId}>Account name</label>
        <input
          id={accountNameId}
          autoFocus
          type="text"
          className="cursor-input"
          value={accountName}
          placeholder="e.g. Personal"
          autoComplete="off"
          required
          onChange={(event) => onAccountNameChange(event.target.value)}
        />
      </div>
    </div>
  )
}
