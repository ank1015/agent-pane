import { Upload04Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useId, useRef, useState } from 'react'
import { Dialog } from '../../components/Dialog'
import { useCreateProject } from './project-queries'

const MAX_AVATAR_BYTES = 512 * 1024
const ACCEPTED_AVATAR_TYPES = new Set([
  'image/gif',
  'image/jpeg',
  'image/png',
  'image/webp',
])

type AddProjectDialogProps = {
  open: boolean
  onClose: () => void
}

export function AddProjectDialog({ open, onClose }: AddProjectDialogProps) {
  const [name, setName] = useState('')
  const [avatar, setAvatar] = useState<string | null>(null)
  const [avatarName, setAvatarName] = useState<string | null>(null)
  const [avatarError, setAvatarError] = useState<string | null>(null)
  const [isReadingAvatar, setIsReadingAvatar] = useState(false)
  const nameId = useId()
  const avatarId = useId()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const avatarSelectionRef = useRef(0)
  const createProject = useCreateProject()
  const resetCreateProject = createProject.reset

  const closeDialog = useCallback(() => {
    avatarSelectionRef.current += 1
    resetCreateProject()
    setName('')
    setAvatar(null)
    setAvatarName(null)
    setAvatarError(null)
    setIsReadingAvatar(false)
    if (fileInputRef.current !== null) {
      fileInputRef.current.value = ''
    }
    onClose()
  }, [onClose, resetCreateProject])

  const selectAvatar = async (file: File | undefined) => {
    const selection = avatarSelectionRef.current + 1
    avatarSelectionRef.current = selection
    createProject.reset()
    setAvatar(null)
    setAvatarName(null)
    setAvatarError(null)

    if (file === undefined) {
      return
    }
    if (!ACCEPTED_AVATAR_TYPES.has(file.type)) {
      setAvatarError('Choose a PNG, JPEG, WebP, or GIF image.')
      if (fileInputRef.current !== null) {
        fileInputRef.current.value = ''
      }
      return
    }
    if (file.size > MAX_AVATAR_BYTES) {
      setAvatarError('Avatar images must be 512 KB or smaller.')
      if (fileInputRef.current !== null) {
        fileInputRef.current.value = ''
      }
      return
    }

    setIsReadingAvatar(true)
    try {
      const dataUrl = await readFileAsDataUrl(file)
      if (avatarSelectionRef.current === selection) {
        setAvatar(dataUrl)
        setAvatarName(file.name)
      }
    } catch {
      if (avatarSelectionRef.current === selection) {
        setAvatarError('The selected image could not be read.')
        if (fileInputRef.current !== null) {
          fileInputRef.current.value = ''
        }
      }
    } finally {
      if (avatarSelectionRef.current === selection) {
        setIsReadingAvatar(false)
      }
    }
  }

  const removeAvatar = () => {
    avatarSelectionRef.current += 1
    createProject.reset()
    setAvatar(null)
    setAvatarName(null)
    setAvatarError(null)
    setIsReadingAvatar(false)
    if (fileInputRef.current !== null) {
      fileInputRef.current.value = ''
    }
  }

  const finish = async () => {
    const normalizedName = name.trim()
    if (
      createProject.isPending ||
      isReadingAvatar ||
      normalizedName.length === 0
    ) {
      return
    }

    try {
      await createProject.mutateAsync({
        name: normalizedName,
        avatar,
      })
      closeDialog()
    } catch {
      // The mutation error is rendered in the dialog.
    }
  }

  return (
    <Dialog
      className="add-project-dialog"
      open={open}
      title="Add Project"
      dismissible={!createProject.isPending}
      onClose={closeDialog}
      footer={
        <button
          type="submit"
          form="add-project-form"
          className="cursor-button add-provider-next-button"
          disabled={
            createProject.isPending ||
            isReadingAvatar ||
            name.trim().length === 0
          }
        >
          {createProject.isPending ? 'Creating...' : 'Finish'}
        </button>
      }
    >
      <form
        id="add-project-form"
        className="add-project-form"
        onSubmit={(event) => {
          event.preventDefault()
          void finish()
        }}
      >
        <div className="add-project-field">
          <label htmlFor={nameId}>Name</label>
          <input
            id={nameId}
            autoFocus
            type="text"
            className="cursor-input"
            value={name}
            placeholder="e.g. Agent Pane"
            autoComplete="off"
            maxLength={128}
            required
            onChange={(event) => {
              createProject.reset()
              setName(event.target.value)
            }}
          />
        </div>

        <div className="add-project-field">
          <span>
            Avatar <span className="add-project-optional">(optional)</span>
          </span>
          <input
            ref={fileInputRef}
            id={avatarId}
            type="file"
            className="visually-hidden"
            accept="image/png,image/jpeg,image/webp,image/gif"
            onChange={(event) => void selectAvatar(event.target.files?.[0])}
          />
          {avatar !== null ? (
            <div className="project-avatar-selection">
              <img src={avatar} alt="Selected project avatar preview" />
              <span title={avatarName ?? undefined}>{avatarName}</span>
              <button
                type="button"
                className="cursor-button cursor-button--ghost"
                disabled={createProject.isPending}
                onClick={removeAvatar}
              >
                Remove
              </button>
            </div>
          ) : (
            <label className="project-avatar-upload" htmlFor={avatarId}>
              <HugeiconsIcon
                icon={Upload04Icon}
                size={18}
                color="currentColor"
                strokeWidth={1.5}
                aria-hidden="true"
              />
              <span>
                {isReadingAvatar ? 'Reading image...' : 'Choose an image'}
              </span>
              <small>PNG, JPEG, WebP, or GIF · max 512 KB</small>
            </label>
          )}
          {avatarError !== null ? (
            <p className="project-create-error" role="alert">
              {avatarError}
            </p>
          ) : null}
        </div>

        {createProject.isError ? (
          <p className="project-create-error" role="alert">
            {createProject.error.message}
          </p>
        ) : null}
      </form>
    </Dialog>
  )
}

function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.addEventListener('load', () => {
      if (typeof reader.result === 'string') {
        resolve(reader.result)
      } else {
        reject(new Error('FileReader returned an unexpected result'))
      }
    })
    reader.addEventListener('error', () => reject(reader.error))
    reader.readAsDataURL(file)
  })
}
