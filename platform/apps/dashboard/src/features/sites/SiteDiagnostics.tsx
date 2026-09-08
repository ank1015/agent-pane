import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getJson } from '../../lib/api-client'
import { sitePath } from './site-queries'
type Invocation = { id: string; status: string; errorCode: string | null; createdAt: string }
export function SiteDiagnostics({ project, site }: { project: string; site: string }) {
  const [selected, setSelected] = useState<string | null>(null)
  const recent = useQuery({ queryKey: ['site-diagnostics', project, site], queryFn: ({ signal }) => getJson<{ items: Invocation[] }>(sitePath(project, site) + '/diagnostics', signal), retry: false, refetchInterval: 5_000 })
  const invocation = useQuery({ queryKey: ['site-invocation', project, site, selected], queryFn: ({ signal }) => getJson<{ status: string; errorCode: string | null; logs: unknown[] }>(sitePath(project, site) + '/invocations/' + selected, signal), enabled: !!selected, retry: false, refetchInterval: query => query.state.data && ['accepted', 'running'].includes(query.state.data.status) ? 2_000 : false })
  return <section><details><summary>Backend activity</summary><p>Recent requests. Open one to inspect its saved logs.</p>
    {recent.isError ? <p role="alert">{recent.error.message} <button onClick={() => void recent.refetch()}>Retry</button></p> : null}
    {recent.data?.items.length === 0 ? <p>No backend requests yet.</p> : null}
    <ul className="site-session-list">{recent.data?.items.slice(0, 5).map(i => <li key={i.id}><button onClick={() => setSelected(i.id)}>{i.errorCode || i.status}</button><time>{new Date(i.createdAt).toLocaleTimeString()}</time></li>)}</ul>
    {selected ? <div className="site-invocation-logs"><button onClick={() => setSelected(null)}>Close logs</button>
      {invocation.isPending ? <p role="status">Loading logs…</p> : null}
      {invocation.isError ? <p role="alert">{invocation.error.message} <button onClick={() => void invocation.refetch()}>Retry</button></p> : null}
      {invocation.data ? <><p>{invocation.data.errorCode || invocation.data.status}</p><pre>{invocation.data.logs?.length ? JSON.stringify(invocation.data.logs, null, 2) : 'No log messages.'}</pre></> : null}
    </div> : null}
  </details></section>
}
