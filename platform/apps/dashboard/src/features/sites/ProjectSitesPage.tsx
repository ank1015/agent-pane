import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from 'react-router-dom'
import { getJson } from '../../lib/api-client'
export type Site = { id: string; name: string; desired_status: string; updated_at: string }
export function ProjectSitesPage() {
  const { projectId = '' } = useParams()
  const query = useQuery({ queryKey: ['project-sites', projectId], queryFn: ({ signal }) => getJson<{ items: Site[] }>(`/api/site-view/projects/${encodeURIComponent(projectId)}/sites`, signal), retry: false, refetchInterval: 30_000 })
  return <>
    <header className="provider-detail-header"><h1 className="cursor-page-title">Sites</h1></header>
    <section className="providers-section" aria-label="Project sites">
      {query.isError ? <p role="alert" className="project-detail-notice">Couldn’t load sites. {query.error.message} <button className="providers-retry-button" onClick={() => void query.refetch()}>Retry</button></p> : null}
      <div className="providers-table-wrap"><table className="providers-table">
        <caption className="visually-hidden">Sites in this project</caption>
        <thead><tr><th scope="col">Name</th><th scope="col">Status</th><th scope="col">Updated</th><th scope="col">Open</th></tr></thead>
        <tbody aria-busy={query.isFetching}>
          {query.isPending ? <tr><td colSpan={4} className="providers-table-message">Loading sites…</td></tr> : null}
          {query.data?.items.length === 0 ? <tr><td colSpan={4} className="providers-table-message">No sites in this project yet.</td></tr> : null}
          {query.data?.items.map(site => <tr key={site.id}>
            <td><span className="provider-name">{site.name}</span></td>
            <td><span className="provider-detail">{site.desired_status === 'ready' ? 'Available' : 'Unavailable'}</span></td>
            <td><span className="provider-detail">{new Date(site.updated_at).toLocaleString()}</span></td>
            <td><Link className="providers-retry-button" to={`/projects/${projectId}/sites/${site.id}/view`} target="_blank" rel="noopener noreferrer" aria-label={`Open ${site.name} in a new tab`}>Open ↗</Link></td>
          </tr>)}
        </tbody>
      </table></div>
    </section>
  </>
}
