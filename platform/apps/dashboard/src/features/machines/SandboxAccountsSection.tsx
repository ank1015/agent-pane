import { useRef, useState } from 'react'
import { AddE2bAccountDialog } from './AddE2bAccountDialog'
import { useE2bAccounts } from './machine-queries'

export function SandboxAccountsSection() {
  const { data, isPending, isError, error, refetch } = useE2bAccounts()
  const [isAdding, setIsAdding] = useState(false)
  const [notice, setNotice] = useState('')
  const addButton = useRef<HTMLButtonElement>(null)

  function closeDialog() {
    setIsAdding(false)
    // Restore focus after the modal has unmounted, including in Strict Mode.
    requestAnimationFrame(() => addButton.current?.focus())
  }

  return (
    <section className="machine-page-section" aria-labelledby="sandboxes-heading">
      <header className="machine-page-section-header">
        <h2 id="sandboxes-heading" className="cursor-page-title">Sandboxes</h2>
        <button ref={addButton} type="button" className="cursor-button provider-add-button" onClick={() => { setNotice(''); setIsAdding(true) }}>
          Add
        </button>
      </header>
      <span className="account-notice" role="status">{notice}</span>
      {isError && (
        <div className="account-error" role="alert">
          {error.message} <button className="providers-retry-button" onClick={() => void refetch()}>Retry</button>
        </div>
      )}
      {data?.length ? (
        <div className="machine-card-grid">
          {data.map((account) => (
            <article key={account.id} className="machine-card" data-active={account.status === 'active' ? 'true' : undefined} aria-label={`${account.name}, ${account.status} E2B account`}>
              <div className="machine-card-visual">
                <img className="machine-card-image" data-illustration="logo" src="/e2b.png" alt="E2B" />
              </div>
              <div className="machine-card-footer">
                <div className="machine-card-identity">
                  <div className="machine-card-title-line"><h3 title={account.name}>{account.name}</h3></div>
                </div>
                {account.status !== 'active' && <span className="account-status">{account.status}</span>}
              </div>
            </article>
          ))}
        </div>
      ) : !isError && (
        <div className="machine-inventory-group-empty" role="status">
          {isPending ? 'Loading sandbox accounts...' : 'No Sandbox Accounts'}
        </div>
      )}
      {isAdding && <AddE2bAccountDialog onClose={closeDialog} onCreated={(name) => { setNotice(`${name} added.`); closeDialog() }} />}
    </section>
  )
}
