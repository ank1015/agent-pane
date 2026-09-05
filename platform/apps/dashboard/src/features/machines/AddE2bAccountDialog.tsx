import { ViewIcon, ViewOffIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useId, useRef, useState, type FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import { useCreateE2bAccount } from './machine-queries'

type AddE2bAccountDialogProps = {
  onClose: () => void
  onCreated: (name: string) => void
}

export function AddE2bAccountDialog({ onClose, onCreated }: AddE2bAccountDialogProps) {
  const formId = useId()
  const inputId = useId()
  const errorId = useId()
  const submitting = useRef(false)
  const inputRef = useRef<HTMLInputElement>(null)
  const [step, setStep] = useState<'account' | 'credentials'>('account')
  const [name, setName] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [showKey, setShowKey] = useState(false)
  const mutation = useCreateE2bAccount()

  function closeDialog() {
    if (!submitting.current) onClose()
  }

  function goBack() {
    if (submitting.current) return
    mutation.reset()
    setShowKey(false)
    setStep('account')
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting.current || !name.trim()) return
    if (step === 'account') {
      setStep('credentials')
      return
    }
    if (!apiKey.trim()) return
    submitting.current = true
    try {
      const account = await mutation.mutateAsync({ name: name.trim(), api_key: apiKey.trim() })
      onCreated(account.name)
    } catch {
      // Render the mutation's sanitized server error; never retry creation automatically.
    } finally {
      setApiKey('')
      setShowKey(false)
      submitting.current = false
    }
  }

  const isNextDisabled = mutation.isPending || !(step === 'account' ? name.trim() : apiKey.trim())

  return (
    <Dialog
      title="Add Sandbox"
      initialFocusRef={inputRef}
      dismissible={!mutation.isPending}
      onClose={closeDialog}
      footer={
        <>
          {step === 'credentials' && (
            <button
              type="button"
              className="cursor-button cursor-button--ghost add-provider-back-button"
              disabled={mutation.isPending}
              onClick={goBack}
            >
              Back
            </button>
          )}
          <button
            type="submit"
            form={formId}
            className="cursor-button add-provider-next-button"
            disabled={isNextDisabled}
          >
            {mutation.isPending ? 'Adding...' : step === 'credentials' ? 'Finish' : 'Next'}
          </button>
        </>
      }
    >
      <form id={formId} onSubmit={submit} aria-busy={mutation.isPending}>
        {step === 'account' ? (
          <div className="provider-account-step" key="account">
            <div className="provider-account-name-field">
              <label htmlFor={inputId}>Account name</label>
              <input
                id={inputId}
                ref={inputRef}
                autoFocus
                name="account-name"
                type="text"
                className="cursor-input"
                value={name}
                onChange={(event) => setName(event.target.value)}
                required
                maxLength={200}
                placeholder="e.g. Personal"
                autoComplete="off"
              />
            </div>
          </div>
        ) : (
          <div className="provider-credentials-step" key="credentials">
            <div className="provider-api-key-field">
              <label htmlFor={inputId}>E2B API key</label>
              <div className="provider-api-key-input-wrap">
                <input
                  id={inputId}
                  ref={inputRef}
                  autoFocus
                  name="api-key"
                  type={showKey ? 'text' : 'password'}
                  className="cursor-input"
                  value={apiKey}
                  onChange={(event) => setApiKey(event.target.value)}
                  required
                  maxLength={4096}
                  disabled={mutation.isPending}
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder="Enter API key"
                  aria-describedby={mutation.isError ? errorId : undefined}
                  aria-invalid={mutation.isError || undefined}
                />
                <button
                  type="button"
                  className="cursor-button cursor-button--ghost cursor-icon-button provider-api-key-visibility"
                  aria-label={showKey ? 'Hide API key' : 'Show API key'}
                  aria-pressed={showKey}
                  disabled={mutation.isPending}
                  onClick={() => setShowKey((visible) => !visible)}
                >
                  <HugeiconsIcon icon={showKey ? ViewIcon : ViewOffIcon} size={17} color="currentColor" strokeWidth={1.5} aria-hidden="true" />
                </button>
              </div>
            </div>
            {mutation.isError && (
              <p id={errorId} className="provider-create-error" role="alert">{mutation.error.message}</p>
            )}
          </div>
        )}
      </form>
    </Dialog>
  )
}
