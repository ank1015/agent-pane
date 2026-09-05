import { Archive03Icon, Loading03Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { NavLink, useLocation, useNavigate } from 'react-router-dom'
import { useArchiveSession, useProjectSessions } from './session-queries'

export function ProjectRecentChats({ projectId }: { projectId: string }) {
  const query = useProjectSessions(projectId)
  const archive = useArchiveSession(projectId)
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const projectPath = `/projects/${encodeURIComponent(projectId)}`
  const sessions = [...new Map((query.data?.pages.flatMap(p => p.items) ?? []).map(s => [s.id, s])).values()]
  return <section className="project-recents" aria-label="Recent Chats">
    <div className="project-recents-header">
      <span className="project-recents-label">Recent Chats</span>
    </div>
    <div className="project-session-list">
      {query.isPending ? query.fetchStatus === 'paused'
        ? <div className="project-session-list-state project-session-placeholder" role="status">You’re offline</div>
        : <div className="project-session-list-state project-session-placeholder" role="status" aria-label="Loading chats">
          <HugeiconsIcon className="project-session-spinner" icon={Loading03Icon} size={15} strokeWidth={1.6} aria-hidden="true" />
        </div>
        : null}
      {query.isSuccess && sessions.length === 0 ? <div className="project-session-list-state project-session-placeholder">No Chats yet</div> : null}
      {query.isError ? <div className="project-session-list-state" role="status">{query.data ? 'Couldn’t refresh chats' : 'Couldn’t load chats'}{' '}<button type="button" className="providers-retry-button" disabled={query.isFetching} onClick={() => void query.refetch()}>Retry</button></div> : null}
      {sessions.map(session => {
        const title = session.title?.trim() || 'Untitled chat'
        const path = `${projectPath}/${encodeURIComponent(session.id)}`
        const selected = pathname.replace(/\/$/, '') === path
        const busy = session.active_run !== null
        const archiving = archive.isPending && archive.variables === session.id
        return <div key={session.id} className={`project-session-item${selected ? ' project-session-item--active' : ''}${busy ? ' project-session-item--running' : ''}`}>
          <NavLink end className="project-session-link" to={path} title={title}><span className="project-session-name">{title}</span></NavLink>
          <span className="project-session-trailing">
            {busy ? <HugeiconsIcon className="project-session-spinner" icon={Loading03Icon} size={15} strokeWidth={1.6} aria-label={session.active_run?.status === 'waiting' ? 'Session is waiting' : session.active_run?.status === 'ready' ? 'Session is queued' : 'Session is running'} /> :
              <button type="button" className="project-session-archive" title="Archive" aria-label={`Archive ${title}`} disabled={archive.isPending} onClick={() => archive.mutate(session.id, { onSuccess: () => { if (selected) navigate(projectPath) } })}>
                <HugeiconsIcon className={archiving ? 'project-session-spinner' : undefined} icon={archiving ? Loading03Icon : Archive03Icon} size={15} strokeWidth={1.5} aria-hidden="true" />
              </button>}
          </span>
        </div>
      })}
      {query.hasNextPage ? <button className="project-session-list-state providers-retry-button" type="button" disabled={query.isFetching} onClick={() => void query.fetchNextPage()}>{query.isFetchingNextPage ? 'Loading…' : 'Load more'}</button> : null}
      {archive.isError ? <div className="project-session-list-state" role="alert">Couldn’t archive chat. Try again.</div> : null}
    </div>
  </section>
}
