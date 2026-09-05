import { ViewIcon, ViewOffIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useQueryClient } from '@tanstack/react-query'
import { useEffect, useId, useRef, useState, type FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import { ProviderIcon } from './provider-icons'
import { PROVIDER_LABELS, PROVIDER_OPTIONS } from './provider-metadata'
import { cancelChatGptLogin, providerKeys, useChatGptLoginStatus, useCreateProviderAccount, useStartChatGptLogin } from './provider-queries'
import type { ProviderKind } from './provider-types'

export function AddProviderDialog({ onClose }: { onClose: () => void }) {
  const formId = useId()
  const inputId = useId()
  const errorId = useId()
  const initialFocus = useRef<HTMLInputElement>(null)
  const submitting = useRef(false)
  const resources = useRef<{ id: string | null; popup: Window | null }>({ id: null, popup: null })
  const [step, setStep] = useState<'provider' | 'account' | 'credentials'>('provider')
  const [provider, setProvider] = useState<ProviderKind | null>(null)
  const [name, setName] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [showKey, setShowKey] = useState(false)
  const [loginId, setLoginId] = useState<string | null>(null)
  const [loginError, setLoginError] = useState<string | null>(null)
  const [canceling, setCanceling] = useState(false)
  const create = useCreateProviderAccount()
  const start = useStartChatGptLogin()
  const login = useChatGptLoginStatus(loginId)
  const client = useQueryClient()
  const busy = create.isPending || start.isPending || canceling || login.data?.status === 'exchanging'
  const waiting = start.isPending || (loginId !== null && !['succeeded', 'failed'].includes(login.data?.status ?? ''))

  useEffect(() => () => {
    resources.current.popup?.close()
    if (resources.current.id) void cancelChatGptLogin(resources.current.id).catch(() => {})
  }, [])

  useEffect(() => {
    if (login.data?.status === 'succeeded') {
      void client.invalidateQueries({ queryKey: providerKeys.accounts() })
      onClose()
    }
  }, [login.data, client, onClose])

  async function leave(back = false) {
    if (busy || submitting.current) return
    submitting.current = true
    setCanceling(true)
    try {
      if (resources.current.id) await cancelChatGptLogin(resources.current.id)
      resources.current.id = null
      resources.current.popup?.close()
      resources.current.popup = null
      setLoginId(null)
      setLoginError(null)
      setApiKey('')
      setShowKey(false)
      create.reset()
      start.reset()
      if (back) setStep(step === 'credentials' ? 'account' : 'provider')
      else onClose()
    } catch (error) {
      setLoginError(error instanceof Error ? error.message : 'Could not cancel sign-in. Try again.')
      void login.refetch()
    } finally {
      submitting.current = false
      setCanceling(false)
    }
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (busy || submitting.current || !provider) return
    if (step === 'provider') { setStep('account'); return }
    if (!name.trim()) return
    if (step === 'account') { setStep('credentials'); return }
    if (provider === 'chatgpt' || !apiKey.trim()) return
    submitting.current = true
    try {
      await create.mutateAsync({ provider, name: name.trim(), api_key: apiKey.trim() })
      onClose()
    } catch {
      // A sanitized server error is rendered below. Creation is never retried automatically.
    } finally {
      setApiKey('')
      setShowKey(false)
      submitting.current = false
    }
  }

  async function signIn() {
    if (busy || submitting.current || waiting || !name.trim()) return
    setLoginError(null)
    const popup = window.open('about:blank', 'agent-pane-chatgpt-login', 'popup,width=560,height=720')
    if (!popup) { setLoginError('Your browser blocked the sign-in window. Allow popups and try again.'); return }
    popup.opener = null
    resources.current.popup?.close()
    resources.current.popup = popup
    submitting.current = true
    try {
      if (resources.current.id) await cancelChatGptLogin(resources.current.id)
      const result = await start.mutateAsync(name.trim())
      resources.current.id = result.login_id
      setLoginId(result.login_id)
      popup.location.replace(result.authorization_url)
      popup.focus()
    } catch (error) {
      popup.close()
      setLoginError(error instanceof Error ? error.message : 'Could not start sign-in. Try again.')
    } finally {
      submitting.current = false
    }
  }

  const error = provider === 'chatgpt'
    ? loginError ?? (login.data?.status === 'failed' ? login.data.error : null) ?? login.error?.message ?? start.error?.message
    : create.error?.message
  const nextDisabled = busy || (step === 'provider' ? !provider : step === 'account' ? !name.trim() : !apiKey.trim())

  return (
    <Dialog title="Add Provider" initialFocusRef={initialFocus} dismissible={!busy} onClose={() => void leave()}
      footer={<>
        {step !== 'provider' && <button type="button" className="cursor-button cursor-button--ghost add-provider-back-button" disabled={busy} onClick={() => void leave(true)}>Back</button>}
        {(step !== 'credentials' || provider !== 'chatgpt') && <button type="submit" form={formId} className="cursor-button add-provider-next-button" disabled={nextDisabled}>{create.isPending ? 'Adding...' : step === 'credentials' ? 'Finish' : 'Next'}</button>}
      </>}>
      <form id={formId} onSubmit={submit} aria-busy={busy}>
        {step === 'provider' && <>
          <div className="provider-selection-spacer" aria-hidden="true" />
          <fieldset className="provider-option-grid">
            <legend className="visually-hidden">Provider</legend>
            {PROVIDER_OPTIONS.map((kind, index) => <label className="provider-option" key={kind}>
              <input ref={index === 0 ? initialFocus : undefined} type="radio" name="provider" value={kind} checked={provider === kind} onChange={() => { if (kind !== provider) setName(''); setProvider(kind) }} />
              <span className="provider-option-card"><span className="provider-option-icon" aria-hidden="true"><ProviderIcon provider={kind} width={18} height={18} /></span><span>{PROVIDER_LABELS[kind]}</span></span>
            </label>)}
          </fieldset>
        </>}
        {step === 'account' && <div className="provider-account-step" key="account"><div className="provider-account-name-field">
          <label htmlFor={inputId}>Account name</label>
          <input id={inputId} autoFocus className="cursor-input" value={name} onChange={(event) => setName(event.target.value)} maxLength={200} required placeholder="e.g. Personal" autoComplete="off" />
        </div></div>}
        {step === 'credentials' && provider === 'chatgpt' && <div className="provider-credentials-step provider-credentials-step--chatgpt">
          <button type="button" className="cursor-button cursor-button--secondary chatgpt-sign-in-button" disabled={waiting || busy} onClick={() => void signIn()}>
            <ProviderIcon provider="chatgpt" width={18} height={18} />{waiting ? 'Waiting for ChatGPT…' : 'Sign In with ChatGPT'}
          </button>
          {error && <p role="alert" className="provider-create-error chatgpt-sign-in-error">{error}</p>}
          {login.isError && <button type="button" className="cursor-button cursor-button--ghost" onClick={() => void login.refetch()}>Check sign-in status</button>}
        </div>}
        {step === 'credentials' && provider && provider !== 'chatgpt' && <div className="provider-credentials-step" key="credentials"><div className="provider-api-key-field">
          <label htmlFor={inputId}>{PROVIDER_LABELS[provider]} API key</label>
          <div className="provider-api-key-input-wrap">
            <input id={inputId} autoFocus className="cursor-input" type={showKey ? 'text' : 'password'} value={apiKey} onChange={(event) => setApiKey(event.target.value)} maxLength={4096} required disabled={busy} autoComplete="off" autoCapitalize="none" spellCheck={false} placeholder={`Enter your ${PROVIDER_LABELS[provider]} API key`} aria-invalid={!!error || undefined} aria-describedby={error ? errorId : undefined} />
            <button type="button" className="cursor-button cursor-button--ghost cursor-icon-button provider-api-key-visibility" aria-label={showKey ? 'Hide API key' : 'Show API key'} aria-pressed={showKey} disabled={busy} onClick={() => setShowKey((visible) => !visible)}>
              <HugeiconsIcon icon={showKey ? ViewIcon : ViewOffIcon} size={16} color="currentColor" strokeWidth={1.5} aria-hidden="true" />
            </button>
          </div>
        </div>{error && <p id={errorId} role="alert" className="provider-create-error">{error}</p>}</div>}
      </form>
    </Dialog>
  )
}
