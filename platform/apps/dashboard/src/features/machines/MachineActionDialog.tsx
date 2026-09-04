import { useId, useRef, useState, type FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import { useMachineAction } from './machine-queries'
import type { ExecutionHost } from './machine-types'

export function MachineActionDialog({ host, action, onClose }: { host: ExecutionHost; action: 'rename' | 'delete'; onClose: () => void }) {
  const currentName = host.name ?? 'Registered Host'
  const [name, setName] = useState(host.name ?? '')
  const mutation = useMachineAction()
  const inputRef = useRef<HTMLInputElement>(null)
  const cancelRef = useRef<HTMLButtonElement>(null)
  const formId = useId()
  const inputId = useId()
  const errorId = useId()
  const normalized = name.trim()
  const valid = normalized.length > 0 && [...normalized].length <= 200 && !/\p{Cc}/u.test(normalized) && normalized !== host.name
  const submitLock = useRef(false)
  const close = () => { if (!mutation.isPending && !submitLock.current) onClose() }
  async function confirm(event?: FormEvent) {
    event?.preventDefault()
    if (submitLock.current || mutation.isPending || (action === 'rename' && !valid)) return
    submitLock.current = true
    try {
      await mutation.mutateAsync(action === 'rename' ? { host, action, name: normalized } : { host, action })
      onClose()
    } catch { /* The dialog retains its inputs and displays the mutation error. */ }
    finally { submitLock.current = false }
  }
  return <Dialog className="machine-action-dialog" title={action === 'rename' ? 'Update name' : `Delete ${currentName}`}
    dismissible={!mutation.isPending} initialFocusRef={action === 'rename' ? inputRef : cancelRef} onClose={close}
    footer={<>
      <button ref={cancelRef} type="button" className="cursor-button cursor-button--ghost" disabled={mutation.isPending} onClick={close}>Cancel</button>
      {action === 'rename'
        ? <button type="submit" form={formId} className="cursor-button" disabled={!valid || mutation.isPending}>{mutation.isPending ? 'Confirming…' : 'Confirm'}</button>
        : <button type="button" className="cursor-button delete-provider-confirm-button" disabled={mutation.isPending} onClick={() => void confirm()}>{mutation.isPending ? 'Deleting…' : 'Delete'}</button>}
    </>}>
    {action === 'rename' ? <form id={formId} onSubmit={(event) => void confirm(event)}>
      <div className="update-provider-name-field"><label htmlFor={inputId}>Machine name</label>
        <input ref={inputRef} id={inputId} className="cursor-input" type="text" autoComplete="off" required maxLength={200}
          value={name} disabled={mutation.isPending} aria-invalid={mutation.isError} aria-describedby={mutation.isError ? errorId : undefined}
          onFocus={(event) => event.currentTarget.select()} onChange={(event) => { setName(event.target.value); mutation.reset() }} />
      </div>
    </form> : <p className="delete-provider-description">This machine will be removed. This action cannot be undone.</p>}
    {mutation.isError ? <p id={errorId} className="update-provider-name-error" role="alert">{mutation.error.message}</p> : null}
  </Dialog>
}
