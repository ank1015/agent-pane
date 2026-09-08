import { useRef, useState } from 'react'
import { RenameDeleteMenu } from '../../components/RenameDeleteMenu'
import { SiteActionDialog } from './SiteActionDialog'
import { Link, useParams } from 'react-router-dom'
import { useSites, type Site } from './site-queries'
import '../../styles/sites.css'
export function ProjectSitesPage() {
  const { projectId = '' } = useParams()
  const query = useSites(projectId)
  const [dialog, setDialog] = useState<{ site: Site; action: 'rename' | 'delete' } | null>(null)
  const trigger = useRef<HTMLButtonElement | null>(null)
  const region = useRef<HTMLDivElement>(null)
  const closeDialog = () => {
    setDialog(null)
    requestAnimationFrame(() => {
      if (trigger.current?.isConnected) trigger.current.focus()
      else region.current?.focus()
    })
  }
  return <>
    <header className="provider-detail-header"><h1 className="cursor-page-title">Sites</h1></header>
    <section className="providers-section" aria-label="Project sites">
      {query.isError ? <p role="alert" className="project-detail-notice">Couldn’t load sites. {query.error.message} <button className="providers-retry-button" onClick={() => void query.refetch()}>Retry</button></p> : null}
      <div ref={region} className="providers-table-wrap" role="region" aria-label="Saved sites" tabIndex={0}><table className="providers-table">
        <caption className="visually-hidden">Sites in this project</caption>
        <thead><tr><th scope="col">Name</th><th scope="col">Status</th><th scope="col">Updated</th><th scope="col">Actions</th><th scope="col" className="provider-actions-column"><span className="visually-hidden">Site options</span></th></tr></thead>
        <tbody aria-live="polite" aria-busy={query.isFetching}>
          {query.isPending ? <tr><td colSpan={5} className="providers-table-message">Loading sites…</td></tr> : null}
          {query.data?.items.length === 0 ? <tr><td colSpan={5} className="providers-table-message">No sites in this project yet.</td></tr> : null}
          {query.data?.items.map(site => <tr key={site.id}>
            <td><Link className="provider-name" to={`/projects/${projectId}/sites/${site.id}`}>{site.name}</Link></td>
            <td><span className="provider-detail">{site.desired_status === 'ready' ? 'Available' : 'Unavailable'}</span></td>
            <td><span className="provider-detail">{new Date(site.updated_at).toLocaleString()}</span></td>
            <td><div className="site-actions"><Link className="providers-retry-button" to={`/projects/${projectId}/sites/${site.id}/view`} target="_blank" rel="noopener noreferrer" aria-label={`Open ${site.name} in a new tab`}>Open ↗</Link></div></td>
            <td className="provider-actions-cell"><RenameDeleteMenu name={site.name} onAction={(action, button) => { trigger.current = button; setDialog({ site, action }) }} /></td>
          </tr>)}
        </tbody>
      </table></div>
    </section>
    {dialog ? <SiteActionDialog key={`${projectId}:${dialog.site.id}:${dialog.action}`} project={projectId} {...dialog} onClose={closeDialog} /> : null}
  </>
}
