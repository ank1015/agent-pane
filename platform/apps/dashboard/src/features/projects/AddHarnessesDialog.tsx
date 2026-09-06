import { useRef, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import type { HarnessUpdateResult, ProjectHarness } from './harness-queries'

export function AddHarnessesDialog({ harnesses, busy, onConfirm, onClose }: {
  harnesses: ProjectHarness[]
  busy: boolean
  onConfirm: (ids: string[]) => Promise<HarnessUpdateResult>
  onClose: () => void
}) {
  const [selected, setSelected] = useState<string[]>([])
  const [errors, setErrors] = useState<string[]>([])
  const lock = useRef(false)
  const cancelRef = useRef<HTMLButtonElement>(null)
  const candidates = harnesses.filter(h => h.project_policy === 'opt_in' && !h.project_enabled)
  const ids = selected.filter(id => candidates.some(h => h.id === id))
  const close = () => { if (!busy && !lock.current) onClose() }

  async function confirm() {
    if (busy || lock.current || !ids.length) return
    lock.current = true
    setErrors([])
    try {
      const result = await onConfirm(ids)
      if (!result.failed.length) onClose()
      else {
        setSelected(result.failed.map(item => item.id))
        setErrors([
          ...(result.updated.length ? [`Added ${result.updated.length} harness${result.updated.length === 1 ? '' : 'es'}.`] : []),
          ...result.failed.map(item => `${harnesses.find(h => h.id === item.id)?.name ?? item.id}: ${item.message}`),
        ])
      }
    } catch (error) {
      setErrors([error instanceof Error ? error.message : 'Could not update harnesses. Try again.'])
    } finally { lock.current = false }
  }

  return <Dialog title="Add harnesses" className="harness-dialog" dismissible={!busy} initialFocusRef={cancelRef} onClose={close}
    footer={<>
      <button ref={cancelRef} type="button" className="cursor-button cursor-button--ghost" disabled={busy} onClick={close}>Cancel</button>
      <button type="button" className="cursor-button" disabled={busy || !ids.length} onClick={() => void confirm()}>{busy ? 'Confirming…' : 'Confirm'}</button>
    </>}>
    <div className="harness-option-list" aria-label="Available harnesses">
      {candidates.map(h => <button key={h.id} type="button" role="switch" aria-checked={selected.includes(h.id)} aria-label={h.name}
        className="harness-option" disabled={busy} onClick={() => {
          setSelected(current => current.includes(h.id) ? current.filter(id => id !== h.id) : [...current, h.id])
          setErrors([])
        }}>
        <span className="provider-name harness-option-copy">{h.name}</span>
        <span className="harness-switch" aria-hidden="true" />
      </button>)}
      {!candidates.length ? <p className="provider-detail">All harnesses have been added.</p> : null}
    </div>
    {errors.length ? <div className="update-provider-name-error" role="alert">{errors.map((error, index) => <p key={index}>{error}</p>)}</div> : null}
  </Dialog>
}
