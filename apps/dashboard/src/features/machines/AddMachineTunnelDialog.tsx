import { Copy01Icon, Tick02Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useEffect, useState } from 'react'
import { Dialog } from '../../components/Dialog'

const REGISTER_COMMAND = `cargo run -p machine-daemon -- --config machine-daemon.json register \\
  --gateway <execution-gateway-url> \\
  --token <registration-token>`

const CONNECT_COMMAND =
  'cargo run -p machine-daemon -- --config machine-daemon.json connect'

type AddMachineTunnelDialogProps = {
  open: boolean
  onClose: () => void
}

export function AddMachineTunnelDialog({
  open,
  onClose,
}: AddMachineTunnelDialogProps) {
  return (
    <Dialog
      open={open}
      title="Add Machine Tunnel"
      className="machine-tunnel-dialog"
      onClose={onClose}
      footer={
        <button type="button" className="cursor-button" onClick={onClose}>
          Done
        </button>
      }
    >
      <div className="machine-tunnel-instructions">
        <p className="machine-tunnel-intro">
          Run these commands on the machine you want to connect. Configure its{' '}
          <code>machine-daemon.json</code> workspace roots before registering.
        </p>

        <ol className="machine-tunnel-steps">
          <li>
            <div className="machine-tunnel-step-copy">
              <span className="machine-tunnel-step-number">1</span>
              <div>
                <h3>Register the machine</h3>
                <p>
                  Use the reachable execution gateway URL and a single-use
                  registration token.
                </p>
              </div>
            </div>
            <CommandBlock label="register" command={REGISTER_COMMAND} />
          </li>

          <li>
            <div className="machine-tunnel-step-copy">
              <span className="machine-tunnel-step-number">2</span>
              <div>
                <h3>Connect the tunnel</h3>
                <p>
                  The daemon uses the machine credential and WebSocket URL saved
                  during registration.
                </p>
              </div>
            </div>
            <CommandBlock label="connect" command={CONNECT_COMMAND} />
          </li>
        </ol>
      </div>
    </Dialog>
  )
}

function CommandBlock({ label, command }: { label: string; command: string }) {
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'failed'>(
    'idle',
  )

  useEffect(() => {
    if (copyState === 'idle') {
      return
    }

    const timeout = window.setTimeout(() => setCopyState('idle'), 2_000)
    return () => window.clearTimeout(timeout)
  }, [copyState])

  const copy = async () => {
    try {
      await copyToClipboard(command)
      setCopyState('copied')
    } catch {
      setCopyState('failed')
    }
  }

  const copied = copyState === 'copied'

  return (
    <div className="machine-tunnel-command">
      <pre>
        <code>{command}</code>
      </pre>
      <button
        type="button"
        className="cursor-button cursor-button--ghost machine-tunnel-copy-button"
        aria-label={`${copied ? 'Copied' : 'Copy'} ${label} command`}
        onClick={() => void copy()}
      >
        <HugeiconsIcon
          icon={copied ? Tick02Icon : Copy01Icon}
          size={14}
          color="currentColor"
          strokeWidth={1.5}
          aria-hidden="true"
        />
        {copyState === 'failed' ? 'Copy failed' : copied ? 'Copied' : 'Copy'}
      </button>
    </div>
  )
}

async function copyToClipboard(value: string): Promise<void> {
  if (navigator.clipboard !== undefined) {
    try {
      await navigator.clipboard.writeText(value)
      return
    } catch {
      // Fall back for browsers that expose Clipboard API without permission.
    }
  }

  const textarea = document.createElement('textarea')
  textarea.value = value
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  document.body.appendChild(textarea)
  textarea.select()
  const copied = document.execCommand('copy')
  textarea.remove()

  if (!copied) {
    throw new Error('Clipboard copy failed')
  }
}
