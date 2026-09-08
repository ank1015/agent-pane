import { useId, useRef, useState, type FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import { useSiteAction, type Site } from './site-queries'

export function SiteActionDialog({ project, site, action, onClose }: { project: string; site: Site; action: 'rename' | 'delete'; onClose: () => void }) {
  const [name, setName] = useState(site.name)
  const mutation = useSiteAction(project)
  const inputRef = useRef<HTMLInputElement>(null)
  const cancelRef = useRef<HTMLButtonElement>(null)
  const submitLock = useRef(false)
  const formId = useId()
  const inputId = useId()
  const errorId = useId()
  const normalized = name.trim()
  const valid = normalized.length > 0 && [...normalized].length <= 128 && !/\p{Cc}/u.test(normalized) && normalized !== site.name
  const close = () => { if (!mutation.isPending && !submitLock.current) onClose() }

  async function confirm(event?: FormEvent) {
    event?.preventDefault()
    if (submitLock.current || mutation.isPending || (action === 'rename' && !valid)) return
    submitLock.current = true
    try {
      await mutation.mutateAsync(action === 'rename'
        ? { siteId: site.id, action, name: normalized }
        : { siteId: site.id, action })
      onClose()
    } catch { /* Keep the dialog and its input available for correction or retry. */ }
    finally { submitLock.current = false }
  }

  return <Dialog className="machine-action-dialog" title={action === 'rename' ? 'Update name' : `Delete ${site.name}`}
    role={action === 'delete' ? 'alertdialog' : 'dialog'}
    dismissible={!mutation.isPending} initialFocusRef={action === 'rename' ? inputRef : cancelRef} onClose={close}
    footer={<>
      <button ref={cancelRef} type="button" className="cursor-button cursor-button--ghost" disabled={mutation.isPending} onClick={close}>Cancel</button>
      {action === 'rename'
        ? <button type="submit" form={formId} className="cursor-button" disabled={!valid || mutation.isPending}>{mutation.isPending ? 'Confirming…' : 'Confirm'}</button>
        : <button type="button" className="cursor-button delete-provider-confirm-button" disabled={mutation.isPending} onClick={() => void confirm()}>{mutation.isPending ? 'Deleting…' : 'Delete'}</button>}
    </>}>
    {action === 'rename' ? <form id={formId} onSubmit={(event) => void confirm(event)}>
      <div className="update-provider-name-field"><label htmlFor={inputId}>Site name</label>
        <input ref={inputRef} id={inputId} className="cursor-input" type="text" autoComplete="off" required
          value={name} disabled={mutation.isPending} aria-invalid={mutation.isError} aria-describedby={mutation.isError ? errorId : undefined}
          onFocus={event => event.currentTarget.select()} onChange={event => { setName(event.target.value); mutation.reset() }} />
      </div>
    </form> : <p className="delete-provider-description">This site will be removed from the list and taken offline. Its files and data will be retained, but it cannot be restored from the dashboard.</p>}
    {mutation.isError ? <p id={errorId} className="update-provider-name-error" role="alert">{mutation.error.message}</p> : null}
  </Dialog>
}
