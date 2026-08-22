import { ViewIcon, ViewOffIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useQueryClient } from '@tanstack/react-query'
import { useCallback, useEffect, useId, useRef, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import { ProviderIcon } from './provider-icons'
import { PROVIDER_LABELS, PROVIDER_OPTIONS } from './provider-metadata'
import {
  type ApiKeyProviderKind,
  type ProviderKind,
  providerKeys,
  useCancelChatGptLogin,
  useChatGptLoginStatus,
  useCreateProviderAccount,
  useStartChatGptLogin,
} from './provider-queries'

type AddProviderStep = 'provider' | 'credentials' | 'account'

type AddProviderDialogProps = {
  open: boolean
  onClose: () => void
}

export function AddProviderDialog({ open, onClose }: AddProviderDialogProps) {
  const [step, setStep] = useState<AddProviderStep>('provider')
  const [selectedProvider, setSelectedProvider] = useState<ProviderKind | null>(
    null,
  )
  const [apiKey, setApiKey] = useState('')
  const [accountName, setAccountName] = useState('')
  const [chatGptLoginId, setChatGptLoginId] = useState<string | null>(null)
  const [chatGptPopupError, setChatGptPopupError] = useState<string | null>(
    null,
  )
  const chatGptPopupRef = useRef<Window | null>(null)
  const queryClient = useQueryClient()
  const createProviderAccount = useCreateProviderAccount()
  const startChatGptLogin = useStartChatGptLogin()
  const cancelChatGptLogin = useCancelChatGptLogin()
  const chatGptLoginStatus = useChatGptLoginStatus(chatGptLoginId)

  const closeDialog = useCallback(() => {
    if (chatGptLoginId !== null) {
      cancelChatGptLogin.mutate(chatGptLoginId)
    }
    chatGptPopupRef.current?.close()
    chatGptPopupRef.current = null
    createProviderAccount.reset()
    startChatGptLogin.reset()
    setStep('provider')
    setSelectedProvider(null)
    setApiKey('')
    setAccountName('')
    setChatGptLoginId(null)
    setChatGptPopupError(null)
    onClose()
  }, [
    cancelChatGptLogin,
    chatGptLoginId,
    createProviderAccount,
    onClose,
    startChatGptLogin,
  ])

  useEffect(() => {
    if (chatGptLoginStatus.data?.status !== 'succeeded') {
      return
    }
    void queryClient.invalidateQueries({ queryKey: providerKeys.accounts() })
    // The remote login completion intentionally drives the dialog lifecycle.
    // oxlint-disable-next-line react/set-state-in-effect
    closeDialog()
  }, [chatGptLoginStatus.data, closeDialog, queryClient])

  const selectProvider = (provider: ProviderKind) => {
    if (provider !== selectedProvider) {
      createProviderAccount.reset()
      startChatGptLogin.reset()
      setApiKey('')
      setAccountName('')
      setChatGptPopupError(null)
    }
    setSelectedProvider(provider)
  }

  const goBack = () => {
    createProviderAccount.reset()
    startChatGptLogin.reset()
    if (chatGptLoginId !== null) {
      cancelChatGptLogin.mutate(chatGptLoginId)
      setChatGptLoginId(null)
    }
    chatGptPopupRef.current?.close()
    chatGptPopupRef.current = null
    setChatGptPopupError(null)
    setStep(
      step === 'account'
        ? 'provider'
        : 'account',
    )
  }

  const goNext = () => {
    if (step === 'provider' && selectedProvider !== null) {
      setStep('account')
    } else if (step === 'account' && selectedProvider !== null) {
      setStep('credentials')
    }
  }

  const isNextDisabled = (() => {
    if (step === 'provider') {
      return selectedProvider === null
    }
    if (step === 'credentials') {
      return apiKey.trim().length === 0
    }
    return accountName.trim().length === 0
  })()

  const finish = async () => {
    if (
      step !== 'credentials' ||
      selectedProvider === null ||
      selectedProvider === 'chatgpt'
    ) {
      return
    }

    try {
      await createProviderAccount.mutateAsync({
        provider: selectedProvider as ApiKeyProviderKind,
        name: accountName.trim(),
        apiKey,
      })
      closeDialog()
    } catch {
      // The mutation state renders the platform or gateway error in the dialog.
    }
  }

  const runPrimaryAction = () => {
    if (step === 'credentials' && selectedProvider !== 'chatgpt') {
      void finish()
    } else {
      goNext()
    }
  }

  const beginChatGptSignIn = async () => {
    if (selectedProvider !== 'chatgpt' || accountName.trim().length === 0) {
      return
    }

    setChatGptPopupError(null)
    startChatGptLogin.reset()
    const popup = window.open(
      'about:blank',
      'agent-pane-chatgpt-login',
      'popup,width=560,height=720',
    )
    if (popup === null) {
      setChatGptPopupError(
        'Your browser blocked the sign-in window. Allow popups and try again.',
      )
      return
    }
    chatGptPopupRef.current = popup

    try {
      const login = await startChatGptLogin.mutateAsync(accountName.trim())
      setChatGptLoginId(login.login_id)
      popup.location.replace(login.authorization_url)
      popup.focus()
    } catch {
      popup.close()
      chatGptPopupRef.current = null
    }
  }

  const chatGptError = (() => {
    if (chatGptPopupError !== null) {
      return chatGptPopupError
    }
    if (startChatGptLogin.isError) {
      return startChatGptLogin.error.message
    }
    if (chatGptLoginStatus.data?.status === 'failed') {
      return chatGptLoginStatus.data.error
    }
    if (chatGptLoginStatus.isError) {
      return chatGptLoginStatus.error.message
    }
    return null
  })()
  const isChatGptWaiting =
    startChatGptLogin.isPending ||
    (chatGptLoginId !== null &&
      chatGptLoginStatus.data?.status !== 'failed' &&
      chatGptLoginStatus.data?.status !== 'succeeded')

  return (
    <Dialog
      open={open}
      title="Add Provider"
      onClose={closeDialog}
      footer={
        <>
          {step !== 'provider' ? (
            <button
              type="button"
              className="cursor-button cursor-button--ghost add-provider-back-button"
              disabled={createProviderAccount.isPending}
              onClick={goBack}
            >
              Back
            </button>
          ) : null}
          {step !== 'credentials' || selectedProvider !== 'chatgpt' ? (
            <button
              type="button"
              className="cursor-button add-provider-next-button"
              disabled={isNextDisabled || createProviderAccount.isPending}
              onClick={runPrimaryAction}
            >
              {step === 'credentials' && selectedProvider !== 'chatgpt'
                ? 'Finish'
                : 'Next'}
            </button>
          ) : null}
        </>
      }
    >
      {step === 'provider' ? (
        <ProviderSelectionStep
          selectedProvider={selectedProvider}
          onSelect={selectProvider}
        />
      ) : null}
      {step === 'credentials' && selectedProvider !== null ? (
        <CredentialsStep
          provider={selectedProvider}
          apiKey={apiKey}
          chatGptError={chatGptError}
          createError={
            selectedProvider !== 'chatgpt' && createProviderAccount.isError
              ? createProviderAccount.error.message
              : null
          }
          isCreating={createProviderAccount.isPending}
          isChatGptWaiting={isChatGptWaiting}
          onApiKeyChange={setApiKey}
          onChatGptSignIn={() => void beginChatGptSignIn()}
        />
      ) : null}
      {step === 'account' && selectedProvider !== null ? (
        <AccountNameStep
          accountName={accountName}
          onAccountNameChange={setAccountName}
        />
      ) : null}
    </Dialog>
  )
}

function AccountNameStep({
  accountName,
  onAccountNameChange,
}: {
  accountName: string
  onAccountNameChange: (accountName: string) => void
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

function ProviderSelectionStep({
  selectedProvider,
  onSelect,
}: {
  selectedProvider: ProviderKind | null
  onSelect: (provider: ProviderKind) => void
}) {
  return (
    <>
      <div className="provider-selection-spacer" aria-hidden="true" />

      <fieldset className="provider-option-grid">
        <legend className="visually-hidden">Provider</legend>
        {PROVIDER_OPTIONS.map((provider) => (
          <label className="provider-option" key={provider.id}>
            <input
              type="radio"
              name="provider"
              value={provider.id}
              checked={selectedProvider === provider.id}
              onChange={() => onSelect(provider.id)}
            />
            <span className="provider-option-card">
              <span className="provider-option-icon" aria-hidden="true">
                <ProviderIcon provider={provider.id} width={18} height={18} />
              </span>
              <span>{provider.label}</span>
            </span>
          </label>
        ))}
      </fieldset>
    </>
  )
}

function CredentialsStep({
  provider,
  apiKey,
  chatGptError,
  createError,
  isCreating,
  isChatGptWaiting,
  onApiKeyChange,
  onChatGptSignIn,
}: {
  provider: ProviderKind
  apiKey: string
  chatGptError: string | null
  createError: string | null
  isCreating: boolean
  isChatGptWaiting: boolean
  onApiKeyChange: (value: string) => void
  onChatGptSignIn: () => void
}) {
  const providerLabel = PROVIDER_LABELS[provider]
  const apiKeyInputId = useId()
  const [isApiKeyVisible, setIsApiKeyVisible] = useState(false)

  if (provider === 'chatgpt') {
    return (
      <div className="provider-credentials-step provider-credentials-step--chatgpt">
        <button
          type="button"
          className="cursor-button cursor-button--secondary chatgpt-sign-in-button"
          disabled={isChatGptWaiting}
          onClick={onChatGptSignIn}
        >
          <ProviderIcon provider="chatgpt" width={18} height={18} />
          {isChatGptWaiting ? 'Waiting for ChatGPT…' : 'Sign In with ChatGPT'}
        </button>
        {chatGptError !== null ? (
          <p className="provider-create-error chatgpt-sign-in-error" role="alert">
            {chatGptError}
          </p>
        ) : null}
      </div>
    )
  }

  return (
    <div className="provider-credentials-step">
      <div className="provider-api-key-field">
        <label htmlFor={apiKeyInputId}>{providerLabel} API key</label>
        <div className="provider-api-key-input-wrap">
          <input
            id={apiKeyInputId}
            autoFocus
            type={isApiKeyVisible ? 'text' : 'password'}
            className="cursor-input"
            value={apiKey}
            placeholder={`Enter your ${providerLabel} API key`}
            autoComplete="off"
            spellCheck={false}
            disabled={isCreating}
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
              size={16}
              color="currentColor"
              strokeWidth={1.5}
              aria-hidden="true"
            />
          </button>
        </div>
      </div>
      {createError !== null ? (
        <p className="provider-create-error" role="alert">
          {createError}
        </p>
      ) : null}
    </div>
  )
}
