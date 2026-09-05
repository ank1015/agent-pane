import { useId, useRef, useState, type FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import { useEnvironmentAction } from './project-queries'
import type { ProjectEnvironment } from './project-types'

export function EnvironmentActionDialog({ environment, action, onClose }: { environment: ProjectEnvironment; action: 'rename' | 'delete'; onClose: () => void }) {
  const [name, setName] = useState(environment.name)
  const mutation = useEnvironmentAction(environment.project_id)
  const inputRef = useRef<HTMLInputElement>(null)
  const cancelRef = useRef<HTMLButtonElement>(null)
  const submitLock = useRef(false)
  const formId = useId()
  const inputId = useId()
  const errorId = useId()
  const normalized = name.trim()
  const valid = normalized.length > 0 && [...normalized].length <= 128 && !/\p{Cc}/u.test(normalized) && normalized !== environment.name
  const close = () => { if (!mutation.isPending && !submitLock.current) onClose() }

  async function confirm(event?: FormEvent) {
    event?.preventDefault()
    if (submitLock.current || mutation.isPending || (action === 'rename' && !valid)) return
    submitLock.current = true
    try {
      await mutation.mutateAsync(action === 'rename'
        ? { environmentId: environment.id, action, name: normalized }
        : { environmentId: environment.id, action })
      onClose()
    } catch { /* Keep the dialog and its input available for correction or retry. */ }
    finally { submitLock.current = false }
  }

  return <Dialog className="machine-action-dialog" title={action === 'rename' ? 'Update name' : `Delete ${environment.name}`}
    role={action === 'delete' ? 'alertdialog' : 'dialog'}
    dismissible={!mutation.isPending} initialFocusRef={action === 'rename' ? inputRef : cancelRef} onClose={close}
    footer={<>
      <button ref={cancelRef} type="button" className="cursor-button cursor-button--ghost" disabled={mutation.isPending} onClick={close}>Cancel</button>
      {action === 'rename'
        ? <button type="submit" form={formId} className="cursor-button" disabled={!valid || mutation.isPending}>{mutation.isPending ? 'Confirming…' : 'Confirm'}</button>
        : <button type="button" className="cursor-button delete-provider-confirm-button" disabled={mutation.isPending} onClick={() => void confirm()}>{mutation.isPending ? 'Deleting…' : 'Delete'}</button>}
    </>}>
    {action === 'rename' ? <form id={formId} onSubmit={(event) => void confirm(event)}>
      <div className="update-provider-name-field"><label htmlFor={inputId}>Environment name</label>
        <input ref={inputRef} id={inputId} className="cursor-input" type="text" autoComplete="off" required
          value={name} disabled={mutation.isPending} aria-invalid={mutation.isError} aria-describedby={mutation.isError ? errorId : undefined}
          onFocus={event => event.currentTarget.select()} onChange={event => { setName(event.target.value); mutation.reset() }} />
      </div>
    </form> : <p className="delete-provider-description">This environment configuration will be removed. This action cannot be undone.</p>}
    {mutation.isError ? <p id={errorId} className="update-provider-name-error" role="alert">{mutation.error.message}</p> : null}
  </Dialog>
}
