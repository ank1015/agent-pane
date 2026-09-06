import { Delete02Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useRef, useState } from 'react'
import { AddHarnessesDialog } from './AddHarnessesDialog'
import { useHarnessUpdate, useProjectHarnesses, type ProjectHarness } from './harness-queries'

export function ProjectHarnessesTable({ projectId }: { projectId: string }) {
  const query = useProjectHarnesses(projectId)
  const update = useHarnessUpdate(projectId)
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const addRef = useRef<HTMLButtonElement>(null)
  const lock = useRef(false)
  const close = useCallback(() => { setAdding(false); addRef.current?.focus() }, [])
  const active = query.data?.items.filter(h => h.project_enabled) ?? []

  async function remove(harness: ProjectHarness) {
    if (lock.current || update.isPending || harness.project_policy === 'required') return
    lock.current = true
    setError(null)
    try {
      const result = await update.mutateAsync({ ids: [harness.id], enabled: false })
      if (result.failed.length) setError(result.failed[0].message)
      else addRef.current?.focus()
    } catch (error) { setError(error instanceof Error ? error.message : 'Could not remove harness.') }
    finally { lock.current = false }
  }

  return <section className="providers-section" aria-labelledby="project-harnesses-title">
    <div className="providers-section-header">
      <h2 id="project-harnesses-title">Active Harnesses</h2>
      <button ref={addRef} type="button" className="cursor-button provider-add-button" disabled={!query.data || update.isPending}
        onClick={() => { setError(null); setAdding(true) }}>Add</button>
    </div>
    {error ? <p className="update-provider-name-error" role="alert">{error}</p> : null}
    <div className="providers-table-wrap" role="region" aria-label="Active harnesses" tabIndex={0}>
      <table className="providers-table project-harnesses-table">
        <colgroup><col /><col className="harness-type-column" /><col className="harness-status-column" /><col className="provider-actions-column" /></colgroup>
        <thead><tr><th scope="col">Name</th><th scope="col">Type</th><th scope="col">Status</th><th scope="col"><span className="visually-hidden">Actions</span></th></tr></thead>
        <tbody aria-busy={query.isPending && query.isFetching}>
          {query.isPending ? <tr><td colSpan={4} className="providers-table-message">{query.isFetching ? 'Loading harnesses…' : 'Waiting for a connection…'}</td></tr> : null}
          {query.isError ? <tr><td colSpan={4} className="providers-table-message"><span role="alert">{query.data ? 'Couldn’t refresh harnesses.' : query.error.message}</span>
            <button type="button" className="providers-retry-button" disabled={query.isFetching} onClick={() => void query.refetch()}>Retry</button></td></tr> : null}
          {query.data && !active.length ? <tr><td colSpan={4} className="providers-table-message">No active harnesses</td></tr> : null}
          {active.map(h => <tr key={h.id}>
            <td><span className="provider-name" title={h.name}>{h.name}</span></td>
            <td><span className="provider-detail">{h.project_policy === 'required' ? 'Platform' : 'Opt-in'}</span></td>
            <td><span className="provider-detail">{h.available ? 'Enabled' : 'Unavailable'}</span></td>
            <td className="provider-actions-cell">{h.project_policy === 'opt_in' ? <button type="button"
              className="cursor-button cursor-button--ghost cursor-icon-button provider-actions-trigger harness-remove-button"
              aria-label={`Remove ${h.name}`} title="Remove harness from project" disabled={update.isPending} onClick={() => void remove(h)}>
              <HugeiconsIcon icon={Delete02Icon} size={16} strokeWidth={1.5} aria-hidden="true" />
            </button> : null}</td>
          </tr>)}
        </tbody>
      </table>
    </div>
    {adding && query.data ? <AddHarnessesDialog harnesses={query.data.items} busy={update.isPending}
      onConfirm={ids => update.mutateAsync({ ids, enabled: true })} onClose={close} /> : null}
  </section>
}
