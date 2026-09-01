import {
  ArrowDown01Icon,
  ArrowUp02Icon,
  FilterHorizontalIcon,
  PlusSignIcon,
  Tick02Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { memo, useId, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'
import { createPortal } from 'react-dom'
import type { HarnessProviderModelOptions } from '../harnesses/harness-queries'
import { ProviderIcon } from '../providers/provider-icons'

type ProjectEnvironmentPromptComposerProps = {
  providerAccounts: readonly HarnessProviderModelOptions[]
  reasoningLevels: readonly string[]
  isModelOptionsPending: boolean
  isModelOptionsError: boolean
  onRetryModelOptions: () => void
  isSubmitting?: boolean
  submitError?: string | null
  onSubmit?: (submission: ProjectEnvironmentPromptSubmission) => void
}

export type ProjectEnvironmentPromptSubmission = {
  prompt: string
  accountId: string
  provider: HarnessProviderModelOptions['provider']
  modelId: string
  reasoningLevel: string
  webSearchEnabled: boolean
}

type AccountTooltip = {
  accountId: string
  name: string
  top: number
  left: number
}

export const ProjectEnvironmentPromptComposer = memo(function ProjectEnvironmentPromptComposer({
  providerAccounts,
  reasoningLevels,
  isModelOptionsPending,
  isModelOptionsError,
  onRetryModelOptions,
  isSubmitting = false,
  submitError = null,
  onSubmit,
}: ProjectEnvironmentPromptComposerProps) {
  const [prompt, setPrompt] = useState('')
  const [model, setModel] = useState<string | null>(null)
  const [accountId, setAccountId] = useState<string | null>(null)
  const [modelPickerOpen, setModelPickerOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [activeModel, setActiveModel] = useState<string | null>(null)
  const [reasoningLevel, setReasoningLevel] = useState<string | null>(null)
  const [webSearchEnabled, setWebSearchEnabled] = useState(true)
  const [accountTooltip, setAccountTooltip] =
    useState<AccountTooltip | null>(null)
  const modelListId = useId()
  const modelTriggerRef = useRef<HTMLButtonElement>(null)
  const settingsTriggerRef = useRef<HTMLButtonElement>(null)
  const selectedAccount =
    providerAccounts.find((account) => account.account_id === accountId) ??
    providerAccounts[0] ??
    null
  const selectedModel =
    selectedAccount !== null &&
    model !== null &&
    selectedAccount.model_ids.includes(model)
      ? model
      : (selectedAccount?.model_ids[0] ?? null)
  const filteredModels = selectedAccount?.model_ids ?? []
  const selectedReasoningLevel =
    reasoningLevel !== null && reasoningLevels.includes(reasoningLevel)
      ? reasoningLevel
      : (reasoningLevels[0] ?? null)
  const reasoningLevelIndex =
    selectedReasoningLevel === null
      ? -1
      : reasoningLevels.indexOf(selectedReasoningLevel)
  const nextReasoningLevel =
    reasoningLevelIndex === -1
      ? null
      : reasoningLevels[(reasoningLevelIndex + 1) % reasoningLevels.length]
  const canSubmit =
    prompt.trim().length > 0 &&
    selectedAccount !== null &&
    selectedModel !== null &&
    selectedReasoningLevel !== null &&
    !isSubmitting

  const submit = () => {
    if (
      !canSubmit ||
      selectedAccount === null ||
      selectedModel === null ||
      selectedReasoningLevel === null
    ) {
      return
    }
    onSubmit?.({
      prompt,
      accountId: selectedAccount.account_id,
      provider: selectedAccount.provider,
      modelId: selectedModel,
      reasoningLevel: selectedReasoningLevel,
      webSearchEnabled,
    })
  }

  const openModelPicker = () => {
    setActiveModel(null)
    setModelPickerOpen(true)
  }

  const closeModelPicker = (restoreFocus = false) => {
    setModelPickerOpen(false)
    setActiveModel(null)
    if (restoreFocus) {
      modelTriggerRef.current?.focus()
    }
  }

  const selectAccount = (account: HarnessProviderModelOptions) => {
    setAccountId(account.account_id)
    setModel(account.model_ids[0] ?? null)
    setActiveModel(null)
  }

  const showAccountTooltip = (
    account: HarnessProviderModelOptions,
    target: HTMLButtonElement,
  ) => {
    const bounds = target.getBoundingClientRect()
    setAccountTooltip({
      accountId: account.account_id,
      name: account.name,
      top: bounds.top + bounds.height / 2,
      left: bounds.left - 8,
    })
  }

  const hideAccountTooltip = (accountId: string) => {
    setAccountTooltip((current) =>
      current?.accountId === accountId ? null : current,
    )
  }

  const selectModel = (modelId: string) => {
    if (selectedAccount !== null) {
      setAccountId(selectedAccount.account_id)
    }
    setModel(modelId)
    closeModelPicker(true)
  }

  const moveActiveModel = (direction: 1 | -1) => {
    if (filteredModels.length === 0) {
      return
    }
    const currentIndex = filteredModels.findIndex(
      (modelId) => modelId === activeModel,
    )
    const nextIndex =
      currentIndex === -1
        ? direction === 1
          ? 0
          : filteredModels.length - 1
        : Math.min(
            Math.max(currentIndex + direction, 0),
            filteredModels.length - 1,
          )
    setActiveModel(filteredModels[nextIndex])
  }

  const handlePickerKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      moveActiveModel(event.key === 'ArrowDown' ? 1 : -1)
    } else if (event.key === 'Enter' && activeModel !== null) {
      const modelId = filteredModels.find((candidate) => candidate === activeModel)
      if (modelId !== undefined) {
        event.preventDefault()
        selectModel(modelId)
      }
    } else if (event.key === 'Escape') {
      event.preventDefault()
      closeModelPicker(true)
    }
  }

  return (
    <section
      className="project-environment-composer"
      aria-label="Environment prompt"
    >
      <textarea
        autoFocus
        className="project-environment-composer-input"
        value={prompt}
        placeholder="Describe the Environment"
        aria-label="Environment instructions"
        spellCheck
        onChange={(event) => setPrompt(event.target.value)}
        onKeyDown={(event) => {
          if (
            event.key === 'Enter' &&
            !event.shiftKey &&
            !event.nativeEvent.isComposing
          ) {
            event.preventDefault()
            submit()
          }
        }}
      />

      <div className="project-environment-composer-footer">
        <button
          type="button"
          className="project-environment-attachment-button"
          aria-label="Add attachment"
          title="Add attachment"
        >
          <HugeiconsIcon
            icon={PlusSignIcon}
            size={15}
            strokeWidth={1.6}
            aria-hidden="true"
          />
        </button>
        <div
          className={`project-environment-model-selector${
            modelPickerOpen ? ' project-environment-model-selector--open' : ''
          }`}
          onBlur={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget)) {
              closeModelPicker()
            }
          }}
        >
          <button
            ref={modelTriggerRef}
            type="button"
            className="project-environment-model-trigger"
            aria-label={
              selectedModel === null
                ? 'No models available'
                : `Model: ${modelLabel(selectedModel)}`
            }
            aria-haspopup="listbox"
            aria-controls={modelPickerOpen ? modelListId : undefined}
            aria-expanded={modelPickerOpen}
            disabled={selectedModel === null}
            onClick={() =>
              modelPickerOpen ? closeModelPicker() : openModelPicker()
            }
            onKeyDown={(event) => {
              if (
                !modelPickerOpen &&
                (event.key === 'ArrowDown' || event.key === 'ArrowUp')
              ) {
                event.preventDefault()
                openModelPicker()
              } else if (modelPickerOpen) {
                handlePickerKeyDown(event)
              }
            }}
          >
            <span>
              {selectedModel === null
                ? isModelOptionsPending
                  ? 'Loading models…'
                  : 'No models available'
                : modelLabel(selectedModel)}
            </span>
            <HugeiconsIcon
              icon={ArrowDown01Icon}
              size={13}
              strokeWidth={1.6}
              aria-hidden="true"
            />
          </button>

          {modelPickerOpen ? (
            <div className="project-environment-model-popover">
              <div className="project-environment-model-picker-body">
                <div
                  className="project-environment-account-sidebar"
                  role="group"
                  aria-label="Provider accounts"
                >
                  {providerAccounts.map((account) => (
                    <button
                      key={account.account_id}
                      type="button"
                      className={`project-environment-account-tab${
                        account.account_id === selectedAccount?.account_id
                          ? ' project-environment-account-tab--active'
                          : ''
                      }`}
                      aria-pressed={
                        account.account_id === selectedAccount?.account_id
                      }
                      aria-label={`${account.name} provider account`}
                      onMouseEnter={(event) =>
                        showAccountTooltip(account, event.currentTarget)
                      }
                      onMouseLeave={() => hideAccountTooltip(account.account_id)}
                      onFocus={(event) =>
                        showAccountTooltip(account, event.currentTarget)
                      }
                      onBlur={() => hideAccountTooltip(account.account_id)}
                      onClick={() => selectAccount(account)}
                    >
                      <ProviderIcon
                        provider={account.provider}
                        width={18}
                        height={18}
                      />
                    </button>
                  ))}
                </div>

                <div
                  id={modelListId}
                  className="project-environment-model-list"
                  role="listbox"
                  aria-label={
                    selectedAccount === null
                      ? 'Models'
                      : `${selectedAccount.name} models`
                  }
                >
                  {filteredModels.length === 0 ? (
                    <p className="project-environment-model-empty">
                      No models available
                    </p>
                  ) : (
                    filteredModels.map((modelId) => (
                      <button
                        key={modelId}
                        type="button"
                        className={`project-environment-model-option${
                          modelId === activeModel
                            ? ' project-environment-model-option--active'
                            : ''
                        }`}
                        role="option"
                        aria-selected={modelId === selectedModel}
                        tabIndex={-1}
                        title={modelId}
                        onMouseEnter={() => setActiveModel(modelId)}
                        onMouseDown={(event) => event.preventDefault()}
                        onClick={() => selectModel(modelId)}
                      >
                        <span className="project-environment-model-option-label">
                          <span>{modelLabel(modelId)}</span>
                        </span>
                        {modelId === selectedModel ? (
                          <HugeiconsIcon
                            icon={Tick02Icon}
                            size={14}
                            strokeWidth={1.8}
                            aria-hidden="true"
                          />
                        ) : null}
                      </button>
                    ))
                  )}
                </div>
              </div>
            </div>
          ) : null}
        </div>
        {isModelOptionsPending ? (
          <span className="visually-hidden" role="status">
            Loading environment options
          </span>
        ) : isModelOptionsError ? (
          <button
            type="button"
            className="project-environment-options-retry"
            onClick={onRetryModelOptions}
          >
            Retry options
          </button>
        ) : selectedReasoningLevel !== null &&
          nextReasoningLevel !== null ? (
          <button
            type="button"
            className="project-environment-reasoning-toggle"
            aria-label={`Reasoning: ${reasoningLabel(selectedReasoningLevel)}. Click to use ${reasoningLabel(nextReasoningLevel)}.`}
            title={`Reasoning: ${reasoningLabel(selectedReasoningLevel)}`}
            onClick={() => setReasoningLevel(nextReasoningLevel)}
          >
            <span
              className="project-environment-reasoning-bars"
              aria-hidden="true"
            >
              {reasoningLevels.map((level, index) => (
                <span
                  key={level}
                  className={
                    index <= reasoningLevelIndex
                      ? 'project-environment-reasoning-bar--active'
                      : undefined
                  }
                  style={{
                    height: reasoningBarHeight(index, reasoningLevels.length),
                  }}
                />
              ))}
            </span>
            <span className="project-environment-reasoning-label">
              <span key={selectedReasoningLevel}>
                {reasoningLabel(selectedReasoningLevel)}
              </span>
            </span>
          </button>
        ) : null}
        <div
          className={`project-environment-settings${
            settingsOpen ? ' project-environment-settings--open' : ''
          }`}
          onBlur={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget)) {
              setSettingsOpen(false)
            }
          }}
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.preventDefault()
              setSettingsOpen(false)
              settingsTriggerRef.current?.focus()
            }
          }}
        >
          <button
            ref={settingsTriggerRef}
            type="button"
            className="project-environment-settings-trigger"
            aria-label="Prompt settings"
            aria-haspopup="dialog"
            aria-expanded={settingsOpen}
            title="Prompt settings"
            onClick={() => setSettingsOpen((open) => !open)}
          >
            <HugeiconsIcon
              icon={FilterHorizontalIcon}
              size={15}
              strokeWidth={1.6}
              aria-hidden="true"
            />
          </button>
          {settingsOpen ? (
            <div
              className="project-environment-settings-popover"
              role="dialog"
              aria-label="Prompt settings"
            >
              <button
                type="button"
                className="project-environment-settings-option"
                role="switch"
                aria-checked={webSearchEnabled}
                onClick={() =>
                  setWebSearchEnabled((enabled) => !enabled)
                }
              >
                <span>Web search</span>
                <span
                  className="project-environment-settings-switch"
                  aria-hidden="true"
                />
              </button>
            </div>
          ) : null}
        </div>
        <button
          type="button"
          className="project-environment-send-button"
          aria-label={isSubmitting ? 'Starting environment run' : 'Send prompt'}
          disabled={!canSubmit}
          onClick={submit}
        >
          <HugeiconsIcon
            icon={ArrowUp02Icon}
            size={17}
            strokeWidth={2}
            aria-hidden="true"
          />
        </button>
      </div>
      {submitError === null ? null : (
        <p className="project-environment-submit-error" role="alert">
          {submitError}
        </p>
      )}
      {accountTooltip === null
        ? null
        : createPortal(
            <div
              className="project-environment-account-tooltip"
              role="tooltip"
              style={{
                top: accountTooltip.top,
                left: accountTooltip.left,
              }}
            >
              {accountTooltip.name}
            </div>,
            document.body,
          )}
    </section>
  )
})

function modelLabel(modelId: string) {
  const leaf = modelId.split('/').at(-1) ?? modelId
  const [family = leaf, ...rest] = leaf.split('-')
  const familyLabel =
    family.toLowerCase() === 'gpt'
      ? 'GPT'
      : family.toLowerCase() === 'deepseek'
        ? 'DeepSeek'
        : `${family.charAt(0).toUpperCase()}${family.slice(1)}`
  const suffix = rest
    .map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`)
    .join(' ')

  if (familyLabel === 'GPT' && rest.length > 0) {
    return `GPT-${rest[0]}${suffix.slice(rest[0].length)}`
  }

  return suffix.length > 0 ? `${familyLabel} ${suffix}` : familyLabel
}

function reasoningLabel(level: string) {
  return level === 'xhigh'
    ? 'Xhigh'
    : `${level.charAt(0).toUpperCase()}${level.slice(1)}`
}

function reasoningBarHeight(index: number, count: number) {
  if (count <= 1) {
    return 11
  }

  const minimumHeight = 4
  const maximumHeight = 11
  return Math.round(
    minimumHeight + (index * (maximumHeight - minimumHeight)) / (count - 1),
  )
}
