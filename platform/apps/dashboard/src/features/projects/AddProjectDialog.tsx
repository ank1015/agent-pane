import { Upload04Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useId, useRef, useState, type FormEvent } from 'react'
import { Dialog } from '../../components/Dialog'
import { useCreateProject } from './project-queries'

const MAX_AVATAR_BYTES = 512 * 1024
const ACCEPTED_AVATAR_TYPES = new Set(['image/gif', 'image/jpeg', 'image/png', 'image/webp'])

export function AddProjectDialog({ onClose }: { onClose: () => void }) {
  const formId = useId()
  const nameId = useId()
  const avatarId = useId()
  const errorId = useId()
  const nameRef = useRef<HTMLInputElement>(null)
  const fileRef = useRef<HTMLInputElement>(null)
  const selection = useRef(0)
  const submitting = useRef(false)
  const [name, setName] = useState('')
  const [avatar, setAvatar] = useState<string | null>(null)
  const [avatarName, setAvatarName] = useState<string | null>(null)
  const [avatarError, setAvatarError] = useState<string | null>(null)
  const [readingAvatar, setReadingAvatar] = useState(false)
  const mutation = useCreateProject()

  function close() {
    if (!submitting.current && !mutation.isPending) onClose()
  }

  async function selectAvatar(file: File | undefined) {
    const current = selection.current + 1
    selection.current = current
    mutation.reset()
    setAvatar(null)
    setAvatarName(null)
    setAvatarError(null)
    if (!file) return
    if (!ACCEPTED_AVATAR_TYPES.has(file.type)) {
      setAvatarError('Choose a PNG, JPEG, WebP, or GIF image.')
      if (fileRef.current) fileRef.current.value = ''
      return
    }
    if (file.size > MAX_AVATAR_BYTES) {
      setAvatarError('Avatar images must be 512 KB or smaller.')
      if (fileRef.current) fileRef.current.value = ''
      return
    }
    setReadingAvatar(true)
    try {
      const value = await readFileAsDataUrl(file)
      if (selection.current === current) {
        setAvatar(value)
        setAvatarName(file.name)
      }
    } catch {
      if (selection.current === current) setAvatarError('The selected image could not be read.')
    } finally {
      if (selection.current === current) setReadingAvatar(false)
    }
  }

  function removeAvatar() {
    selection.current += 1
    mutation.reset()
    setAvatar(null)
    setAvatarName(null)
    setAvatarError(null)
    setReadingAvatar(false)
    if (fileRef.current) fileRef.current.value = ''
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const normalizedName = name.trim()
    if (submitting.current || mutation.isPending || readingAvatar || !normalizedName) return
    submitting.current = true
    try {
      await mutation.mutateAsync({ name: normalizedName, avatar })
      onClose()
    } catch {
      // The sanitized API error is rendered in the dialog.
    } finally {
      setAvatar(null)
      submitting.current = false
    }
  }

  const error = avatarError ?? mutation.error?.message
  return (
    <Dialog
      title="Add Project"
      initialFocusRef={nameRef}
      dismissible={!mutation.isPending && !readingAvatar}
      onClose={close}
      footer={
        <button type="submit" form={formId} className="cursor-button add-provider-next-button" disabled={mutation.isPending || readingAvatar || !name.trim()}>
          {mutation.isPending ? 'Creating...' : 'Finish'}
        </button>
      }
    >
      <form id={formId} className="add-project-form" onSubmit={submit} aria-busy={mutation.isPending || readingAvatar}>
        <div className="add-project-field">
          <label htmlFor={nameId}>Name</label>
          <input id={nameId} ref={nameRef} type="text" className="cursor-input" value={name} placeholder="e.g. Agent Pane" autoComplete="off" maxLength={128} required aria-invalid={mutation.isError || undefined} aria-describedby={error ? errorId : undefined} onChange={(event) => { mutation.reset(); setName(event.target.value) }} />
        </div>
        <div className="add-project-field">
          <span>Avatar <span className="add-project-optional">(optional)</span></span>
          <input ref={fileRef} id={avatarId} type="file" className="visually-hidden" accept="image/png,image/jpeg,image/webp,image/gif" onChange={(event) => void selectAvatar(event.target.files?.[0])} />
          {avatar ? (
            <div className="project-avatar-selection">
              <img src={avatar} alt="Selected project avatar preview" />
              <span title={avatarName ?? undefined}>{avatarName}</span>
              <button type="button" className="cursor-button cursor-button--ghost" disabled={mutation.isPending} onClick={removeAvatar}>Remove</button>
            </div>
          ) : (
            <label className="project-avatar-upload" htmlFor={avatarId}>
              <HugeiconsIcon icon={Upload04Icon} size={18} color="currentColor" strokeWidth={1.5} aria-hidden="true" />
              <span>{readingAvatar ? 'Reading image...' : 'Choose an image'}</span>
              <small>PNG, JPEG, WebP, or GIF · max 512 KB</small>
            </label>
          )}
          {error && <p id={errorId} className="project-create-error" role="alert">{error}</p>}
        </div>
      </form>
    </Dialog>
  )
}

function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.addEventListener('load', () => typeof reader.result === 'string' ? resolve(reader.result) : reject(new Error('Unexpected file result')))
    reader.addEventListener('error', () => reject(reader.error))
    reader.readAsDataURL(file)
  })
}
