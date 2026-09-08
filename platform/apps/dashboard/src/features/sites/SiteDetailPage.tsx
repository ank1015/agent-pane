import { useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getJson, patchJson } from '../../lib/api-client'
import { siteKeys, sitePath, useSite } from './site-queries'
import { useSiteOperation } from './site-operations'
import { SiteDiagnostics } from './SiteDiagnostics'
import { SitePreview } from './SiteViewerPage'
import '../../styles/sites.css'
type Snapshot = { id: string; name: string; release_id: string; created_at: string }
type AuthoringSession = { id: string; title: string | null; active_run: { status: string } | null }
export function SiteDetailPage() {
  const { projectId = '', siteId = '' } = useParams()
  return <SiteDetail key={`${projectId}:${siteId}`} project={projectId} site={siteId} />
}
function SiteDetail({ project, site }: { project: string; site: string }) {
  const detail = useSite(project, site), client = useQueryClient()
  const [snapshotName, setSnapshotName] = useState(''), [name, setName] = useState<string | null>(null), [restore, setRestore] = useState<Snapshot | null>(null)
  const operation = useSiteOperation(project, site)
  const snapshots = useInfiniteQuery({ queryKey: siteKeys.snapshots(project, site), initialPageParam: null as string | null, queryFn: ({ pageParam, signal }) => getJson<{ items: Snapshot[]; nextAfter: string | null }>(`${sitePath(project, site)}/snapshots?limit=20${pageParam ? `&after=${pageParam}` : ''}`, signal), getNextPageParam: (page, _pages, _param, params) => page.nextAfter && !params.includes(page.nextAfter) ? page.nextAfter : undefined, retry: false })
  const sessions = useQuery({ queryKey: ['site-sessions', project, site], queryFn: ({ signal }) => getJson<{ items: AuthoringSession[] }>(`${sitePath(project, site)}/sessions`, signal), retry: false, refetchInterval: 5_000 })
  const rename = useMutation({ mutationFn: () => patchJson(sitePath(project, site), { name: name?.trim() }), onSuccess: () => { setName(null); void client.invalidateQueries({ queryKey: siteKeys.detail(project, site) }); void client.invalidateQueries({ queryKey: siteKeys.list(project) }) } })
  const resource = detail.data?.resource, live = !!resource?.active_release_id && detail.data?.site.desired_status === 'ready'
  return <div className="site-detail-page">
    <header className="provider-detail-header site-detail-header"><div><Link to={`/projects/${project}/sites`} className="provider-detail">Sites</Link><h1 className="cursor-page-title">{detail.data?.site.name || 'Site'}</h1></div><div className="site-actions"><Link className="providers-retry-button" to={`/projects/${project}?harness=sites&site=${site}`}>Edit with Sites</Link>{live ? <Link className="providers-retry-button" to={`/projects/${project}/sites/${site}/view`} target="_blank" rel="noopener noreferrer">Open ↗</Link> : null}</div></header>
    {detail.isError ? <p role="alert">{detail.error.message} <button onClick={() => void detail.refetch()}>Retry</button></p> : null}
    <div className="site-detail-grid"><section className="site-preview-panel" aria-label="Live preview"><div className="site-section-heading"><h2>Live site</h2><span role="status">{detail.isPending ? 'Loading…' : live ? 'Live · changes refresh automatically' : resource?.status === 'ready' ? 'Waiting for the first edit' : resource?.status || 'Unavailable'}</span></div>
      {live ? <SitePreview projectId={project} siteId={site} revision={resource!.active_release_id!} embedded /> : <p className="site-empty">{detail.data?.site.desired_status !== 'ready' && detail.data ? 'This site is unavailable.' : 'Start an authoring chat. The first successful edit makes the site live.'}</p>}
    </section><aside className="site-management">
      <section><h2>Site name</h2><form onSubmit={e => { e.preventDefault(); rename.mutate() }}><label className="visually-hidden" htmlFor="site-name">Site name</label><input id="site-name" maxLength={128} value={name ?? detail.data?.site.name ?? ''} onChange={e => setName(e.target.value)} /><button disabled={rename.isPending || !name?.trim()}>Save name</button></form>{rename.isError ? <p role="alert">{rename.error.message}</p> : null}</section>
      <section><h2>Snapshots</h2><p>Save code you want to keep. Restoring replaces the live frontend and backend. Data and running work stay as they are.</p><form onSubmit={e => { e.preventDefault(); operation.snapshot(snapshotName) }}><label className="visually-hidden" htmlFor="snapshot-name">Snapshot name</label><input id="snapshot-name" placeholder="Snapshot name" maxLength={128} value={snapshotName} onChange={e => setSnapshotName(e.target.value)} disabled={operation.busy} /><button disabled={!live || operation.busy || !snapshotName.trim() || new TextEncoder().encode(snapshotName.trim()).length > 128}>Save snapshot</button></form>
      {operation.busy ? <p role="status">{operation.submitting ? 'Saving…' : <>Checking the saved operation… <button onClick={operation.retry}>Retry the same operation</button></>}</p> : null}
      {operation.notice ? <p role="status">{operation.notice}</p> : null}{operation.error ? <p role="alert">{operation.error}</p> : null}
      {snapshots.isError ? <p role="alert">{snapshots.error.message} <button onClick={() => void snapshots.refetch()}>Retry</button></p> : null}
      <ul className="site-snapshot-list">{snapshots.data?.pages.flatMap(p => p.items).map(s => <li key={s.id}><div><strong>{s.name}</strong><time>{new Date(s.created_at).toLocaleString()}</time></div><button disabled={operation.busy || !live} onClick={() => setRestore(s)}>Restore</button></li>)}</ul>
      {snapshots.data?.pages[0]?.items.length === 0 ? <p>No snapshots yet.</p> : null}
      {snapshots.hasNextPage ? <button disabled={snapshots.isFetchingNextPage} onClick={() => void snapshots.fetchNextPage()}>Load more snapshots</button> : null}
      {restore ? <div role="group" aria-label="Confirm restore" className="site-restore-confirm"><p>Replace the live code with “{restore.name}”? Unsaved code will be replaced. Agents may continue editing.</p><button onClick={() => setRestore(null)}>Cancel</button><button disabled={operation.busy} onClick={() => { operation.restore(restore.id); setRestore(null) }}>Restore snapshot</button></div> : null}
      </section>
      <section><h2>Authoring chats</h2><p>Recent chats working on this site. Multiple chats can edit its live code.</p>{sessions.isError ? <p role="alert">{sessions.error.message} <button onClick={() => void sessions.refetch()}>Retry</button></p> : null}<ul className="site-session-list">{sessions.data?.items.map(s => <li key={s.id}><Link to={`/projects/${project}/${s.id}`}>{s.title || 'Untitled chat'}</Link><span>{s.active_run?.status || 'Idle'}</span></li>)}</ul>{sessions.data?.items.length === 0 ? <p>No authoring chats yet.</p> : null}</section>
      <SiteDiagnostics project={project} site={site} />
    </aside></div>
  </div>
}
