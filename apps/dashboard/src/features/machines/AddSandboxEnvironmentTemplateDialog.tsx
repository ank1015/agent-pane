import { ArrowDown01Icon, Tick02Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useId, useMemo, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import {
  type SandboxSnapshot,
  useCreateSandboxEnvironmentTemplate,
} from './machine-queries'

type AddTemplateStep = 'name' | 'snapshot' | 'path' | 'setup'

type AddSandboxEnvironmentTemplateDialogProps = {
  accountId: string
  open: boolean
  snapshots: SandboxSnapshot[]
  snapshotsError: boolean
  snapshotsPending: boolean
  onClose: () => void
}

export function AddSandboxEnvironmentTemplateDialog({
  accountId,
  open,
  snapshots,
  snapshotsError,
  snapshotsPending,
  onClose,
}: AddSandboxEnvironmentTemplateDialogProps) {
  const [step, setStep] = useState<AddTemplateStep>('name')
  const [name, setName] = useState('')
  const [snapshotId, setSnapshotId] = useState('')
  const [snapshotQuery, setSnapshotQuery] = useState('')
  const [snapshotMenuOpen, setSnapshotMenuOpen] = useState(false)
  const [activeSnapshotIndex, setActiveSnapshotIndex] = useState(-1)
  const [path, setPath] = useState('')
  const [setupScript, setSetupScript] = useState('')
  const nameId = useId()
  const snapshotIdId = useId()
  const snapshotSearchId = useId()
  const snapshotListId = useId()
  const pathId = useId()
  const pathPrefixId = useId()
  const setupScriptId = useId()
  const createTemplate = useCreateSandboxEnvironmentTemplate()
  const resetCreateTemplate = createTemplate.reset

  const normalizedName = name.trim()
  const normalizedSnapshotId = snapshotId.trim()
  const normalizedSnapshotQuery = snapshotQuery.trim()
  const normalizedRelativePath = path.trim().replace(/^\/+/, '')
  const normalizedPath = `/${normalizedRelativePath}`
  const filteredSnapshots = useMemo(() => {
    const query = normalizedSnapshotQuery.toLocaleLowerCase()
    if (query.length === 0) {
      return snapshots
    }
    return snapshots.filter(
      (snapshot) =>
        snapshot.name.toLocaleLowerCase().includes(query) ||
        snapshot.provider_snapshot_id.toLocaleLowerCase().includes(query) ||
        snapshot.id.toLocaleLowerCase().includes(query),
    )
  }, [normalizedSnapshotQuery, snapshots])
  const selectedSnapshot = snapshots.find(
    (snapshot) =>
      snapshot.provider_snapshot_id === normalizedSnapshotId ||
      snapshot.id === normalizedSnapshotId,
  )
  const activeSnapshot =
    filteredSnapshots[
      Math.min(activeSnapshotIndex, filteredSnapshots.length - 1)
    ]

  const closeDialog = useCallback(() => {
    resetCreateTemplate()
    setStep('name')
    setName('')
    setSnapshotId('')
    setSnapshotQuery('')
    setSnapshotMenuOpen(false)
    setActiveSnapshotIndex(-1)
    setPath('')
    setSetupScript('')
    onClose()
  }, [onClose, resetCreateTemplate])

  const finish = async () => {
    if (
      createTemplate.isPending ||
      normalizedName.length === 0 ||
      selectedSnapshot === undefined
    ) {
      return
    }

    try {
      await createTemplate.mutateAsync({
        accountId,
        name: normalizedName,
        snapshotId: selectedSnapshot.id,
        path: normalizedPath,
        setupScript,
      })
      closeDialog()
    } catch {
      // The mutation error is rendered on the setup step.
    }
  }

  const runPrimaryAction = () => {
    if (step === 'name') {
      if (normalizedName.length > 0) {
        setStep('snapshot')
      }
      return
    }
    if (step === 'snapshot') {
      if (selectedSnapshot !== undefined) {
        setSnapshotMenuOpen(false)
        setStep('path')
      }
      return
    }
    if (step === 'path') {
      setStep('setup')
      return
    }

    void finish()
  }

  const goBack = () => {
    createTemplate.reset()
    setSnapshotMenuOpen(false)
    setStep((currentStep) => {
      if (currentStep === 'setup') {
        return 'path'
      }
      return currentStep === 'path' ? 'snapshot' : 'name'
    })
  }

  const selectSnapshot = (snapshot: SandboxSnapshot) => {
    setSnapshotId(snapshot.provider_snapshot_id)
    setSnapshotQuery('')
    setSnapshotMenuOpen(false)
    setActiveSnapshotIndex(-1)
  }

  const primaryDisabled =
    createTemplate.isPending ||
    (step === 'name'
      ? normalizedName.length === 0
      : step === 'snapshot'
        ? selectedSnapshot === undefined
        : false)

  return (
    <Dialog
      className="add-template-dialog"
      open={open}
      title="Add Environment Template"
      dismissible={!createTemplate.isPending}
      onClose={closeDialog}
      footer={
        <>
          {step !== 'name' ? (
            <button
              type="button"
              className="cursor-button cursor-button--ghost add-environment-back-button"
              disabled={createTemplate.isPending}
              onClick={goBack}
            >
              Back
            </button>
          ) : null}
          <button
            type="button"
            className="cursor-button add-environment-next-button"
            disabled={primaryDisabled}
            onClick={runPrimaryAction}
          >
            {step === 'setup'
              ? createTemplate.isPending
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
              placeholder="e.g. TypeScript workspace"
              autoComplete="off"
              maxLength={120}
              required
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && normalizedName.length > 0) {
                  event.preventDefault()
                  setStep('snapshot')
                }
              }}
            />
          </div>
        ) : null}

        {step === 'snapshot' ? (
          <div className="add-environment-field">
            <label htmlFor={snapshotIdId}>Snapshot</label>
            <div
              className={`snapshot-picker-combobox${
                snapshotMenuOpen ? ' snapshot-picker-combobox--open' : ''
              }`}
              onBlur={(event) => {
                if (!event.currentTarget.contains(event.relatedTarget)) {
                  setSnapshotMenuOpen(false)
                }
              }}
            >
              <button
                id={snapshotIdId}
                autoFocus
                type="button"
                className="snapshot-picker-trigger"
                aria-haspopup="listbox"
                aria-controls={snapshotMenuOpen ? snapshotListId : undefined}
                aria-expanded={snapshotMenuOpen}
                disabled={
                  snapshotsPending || snapshotsError || snapshots.length === 0
                }
                onClick={() => setSnapshotMenuOpen((isOpen) => !isOpen)}
                onKeyDown={(event) => {
                  if (event.key === 'ArrowDown' && snapshots.length > 0) {
                    event.preventDefault()
                    setSnapshotMenuOpen(true)
                    setActiveSnapshotIndex(0)
                  }
                }}
              >
                <span
                  className={`snapshot-picker-trigger-label${
                    selectedSnapshot === undefined
                      ? ' snapshot-picker-trigger-label--placeholder'
                      : ''
                  }`}
                >
                  {selectedSnapshot?.name ?? 'Select a snapshot'}
                </span>
                <HugeiconsIcon
                  icon={ArrowDown01Icon}
                  size={14}
                  strokeWidth={1.8}
                  aria-hidden="true"
                />
              </button>
              {snapshotMenuOpen &&
              !snapshotsPending &&
              !snapshotsError &&
              snapshots.length > 0 ? (
                <div
                  className="snapshot-picker-popover"
                  role="presentation"
                >
                  <input
                    id={snapshotSearchId}
                    autoFocus
                    type="text"
                    className="snapshot-picker-search"
                    value={snapshotQuery}
                    placeholder="Search snapshots"
                    autoComplete="off"
                    role="combobox"
                    aria-label="Search snapshots"
                    aria-autocomplete="list"
                    aria-controls={snapshotListId}
                    aria-expanded="true"
                    aria-activedescendant={
                      activeSnapshot === undefined
                        ? undefined
                        : `${snapshotListId}-${activeSnapshot.id}`
                    }
                    onChange={(event) => {
                      setSnapshotQuery(event.target.value)
                      setActiveSnapshotIndex(-1)
                    }}
                    onKeyDown={(event) => {
                      if (
                        event.key === 'ArrowDown' &&
                        filteredSnapshots.length > 0
                      ) {
                        event.preventDefault()
                        setActiveSnapshotIndex((currentIndex) =>
                          Math.min(
                            currentIndex + 1,
                            filteredSnapshots.length - 1,
                          ),
                        )
                      } else if (
                        event.key === 'ArrowUp' &&
                        filteredSnapshots.length > 0
                      ) {
                        event.preventDefault()
                        setActiveSnapshotIndex((currentIndex) =>
                          Math.max(currentIndex - 1, 0),
                        )
                      } else if (event.key === 'Escape') {
                        setSnapshotMenuOpen(false)
                      } else if (
                        event.key === 'Enter' &&
                        activeSnapshot !== undefined
                      ) {
                        event.preventDefault()
                        selectSnapshot(activeSnapshot)
                      }
                    }}
                  />
                  <div
                    id={snapshotListId}
                    className="snapshot-picker-list"
                    role="listbox"
                    aria-label="Snapshots"
                  >
                    {filteredSnapshots.length === 0 ? (
                      <p className="snapshot-picker-empty">
                        No snapshots match your search.
                      </p>
                    ) : (
                      filteredSnapshots.map((snapshot, index) => (
                        <button
                          key={snapshot.id}
                          id={`${snapshotListId}-${snapshot.id}`}
                          type="button"
                          role="option"
                          className={`snapshot-picker-option${
                            selectedSnapshot?.id === snapshot.id
                              ? ' snapshot-picker-option--selected'
                              : ''
                          }${
                            activeSnapshot?.id === snapshot.id
                              ? ' snapshot-picker-option--active'
                              : ''
                          }`}
                          aria-selected={selectedSnapshot?.id === snapshot.id}
                          tabIndex={-1}
                          onMouseEnter={() => setActiveSnapshotIndex(index)}
                          onMouseDown={(event) => event.preventDefault()}
                          onClick={() => selectSnapshot(snapshot)}
                        >
                          <span
                            className="snapshot-picker-name"
                            title={snapshot.name}
                          >
                            {snapshot.name}
                          </span>
                          {selectedSnapshot?.id === snapshot.id ? (
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
              ) : null}
            </div>
            {snapshotsPending ? (
              <p className="provider-create-hint">Loading snapshots…</p>
            ) : null}
            {snapshotsError ? (
              <p className="provider-create-error">
                Couldn&apos;t load snapshots for this account.
              </p>
            ) : null}
            {!snapshotsPending && !snapshotsError && snapshots.length === 0 ? (
              <p className="provider-create-hint">
                Create a snapshot before adding an environment template.
              </p>
            ) : null}
          </div>
        ) : null}

        {step === 'path' ? (
          <div className="add-environment-field">
            <label htmlFor={pathId}>
              Path <span className="setup-script-optional">(optional)</span>
            </label>
            <div className="add-environment-path-input">
              <span
                id={pathPrefixId}
                className="add-environment-path-prefix"
                title="Sandbox root"
              >
                /
              </span>
              <input
                id={pathId}
                autoFocus
                type="text"
                className="cursor-input"
                value={path}
                placeholder="optional/path"
                aria-describedby={pathPrefixId}
                autoComplete="off"
                onChange={(event) =>
                  setPath(event.target.value.replace(/^\/+/, ''))
                }
                onKeyDown={(event) => {
                  if (event.key === 'Enter') {
                    event.preventDefault()
                    setStep('setup')
                  }
                }}
              />
            </div>
          </div>
        ) : null}

        {step === 'setup' ? (
          <div className="add-environment-field">
            <label htmlFor={setupScriptId}>
              Setup script{' '}
              <span className="setup-script-optional">(optional)</span>
            </label>
            <textarea
              id={setupScriptId}
              autoFocus
              className="setup-script-textarea"
              value={setupScript}
              placeholder={'npm install\nnpm run build'}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => setSetupScript(event.target.value)}
            />
            {createTemplate.isError ? (
              <p className="provider-create-error">
                {createTemplate.error.message}
              </p>
            ) : null}
          </div>
        ) : null}
      </div>
    </Dialog>
  )
}
