import {
  ArrowLeft01Icon,
  DashboardSquare01Icon,
  Message01Icon,
  Search01Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useDeferredValue, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { Link, NavLink } from 'react-router-dom'
import { SessionVisualizerPage } from './SessionVisualizerPage'
import { type SessionListItem, useSessions } from './session-queries'
import './sessions.css'

type SessionsPageProps = {
  sessionId?: string
}

const DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
  timeStyle: 'short',
})

const NUMBER_FORMATTER = new Intl.NumberFormat()

export function SessionsPage({ sessionId }: SessionsPageProps) {
  const sessions = useSessions()
  const [search, setSearch] = useState('')
  const deferredSearch = useDeferredValue(search)
  const filteredSessions = useMemo(
    () => filterSessions(sessions.data ?? [], deferredSearch),
    [deferredSearch, sessions.data],
  )

  return (
    <div className="cursor-shell machine-detail-shell sessions-shell">
      <aside className="cursor-sidebar dashboard-sidebar machine-detail-sidebar sessions-sidebar">
        <Link
          className="cursor-nav-item dashboard-nav-item machine-detail-back"
          to="/projects"
        >
          <span className="nav-icon-frame" aria-hidden="true">
            <HugeiconsIcon
              className="nav-item-icon"
              icon={ArrowLeft01Icon}
              size={16}
              color="currentColor"
              strokeWidth={1.5}
            />
          </span>
          <span className="nav-item-label">Back to Dashboard</span>
        </Link>

        <nav className="cursor-nav dashboard-nav sessions-navigation" aria-label="Sessions">
          <NavLink
            end
            className={({ isActive }) =>
              `cursor-nav-item dashboard-nav-item machine-detail-nav-item${
                isActive ? ' dashboard-nav-item--active' : ''
              }`
            }
            to="/sessions"
          >
            <span className="nav-icon-frame" aria-hidden="true">
              <HugeiconsIcon
                className="nav-item-icon"
                icon={DashboardSquare01Icon}
                size={16}
                color="currentColor"
                strokeWidth={1.5}
              />
            </span>
            <span className="nav-item-label">Overview</span>
          </NavLink>
        </nav>

        <div className="sessions-search">
          <HugeiconsIcon
            aria-hidden="true"
            icon={Search01Icon}
            size={14}
            color="currentColor"
            strokeWidth={1.5}
          />
          <input
            aria-label="Search sessions"
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Search sessions"
            type="search"
            value={search}
          />
        </div>

        <div className="sessions-sidebar-heading">
          <span>All sessions</span>
          {sessions.data === undefined ? null : <small>{sessions.data.length}</small>}
        </div>

        <div
          className="sessions-sidebar-list"
          aria-busy={sessions.isPending}
          aria-live="polite"
        >
          {sessions.isPending ? (
            <SidebarState>Loading sessions…</SidebarState>
          ) : sessions.isError ? (
            <SidebarState>
              Couldn’t load sessions.
              <button type="button" onClick={() => void sessions.refetch()}>Retry</button>
            </SidebarState>
          ) : filteredSessions.length === 0 ? (
            <SidebarState>
              {search.trim().length > 0 ? 'No matching sessions.' : 'No sessions yet.'}
            </SidebarState>
          ) : (
            filteredSessions.map((session) => (
              <SessionSidebarLink key={session.id} session={session} />
            ))
          )}
        </div>
      </aside>

      <main className="cursor-main machine-detail-main sessions-main">
        {sessionId === undefined ? (
          <SessionsOverview
            error={sessions.isError ? sessions.error.message : undefined}
            loading={sessions.isPending}
            onRetry={() => void sessions.refetch()}
            sessions={sessions.data ?? []}
          />
        ) : (
          <SessionVisualizerPage sessionId={sessionId} />
        )}
      </main>
    </div>
  )
}

function SessionSidebarLink({ session }: { session: SessionListItem }) {
  return (
    <NavLink
      className={({ isActive }) =>
        `sessions-sidebar-session${isActive ? ' sessions-sidebar-session--active' : ''}`
      }
      title={session.title}
      to={`/sessions/${encodeURIComponent(session.id)}`}
    >
      <span className="sessions-sidebar-session-icon" aria-hidden="true">
        <HugeiconsIcon
          icon={Message01Icon}
          size={14}
          color="currentColor"
          strokeWidth={1.5}
        />
      </span>
      <span className="sessions-sidebar-session-copy">
        <strong>{session.title}</strong>
        <small>{session.project_name} · {formatHarness(session.harness_id)}</small>
      </span>
      {session.is_active ? (
        <span className="sessions-live-dot" title="Active session" />
      ) : null}
    </NavLink>
  )
}

function SessionsOverview({
  error,
  loading,
  onRetry,
  sessions,
}: {
  error?: string
  loading: boolean
  onRetry: () => void
  sessions: readonly SessionListItem[]
}) {
  const activeCount = sessions.filter((session) => session.is_active).length
  const projectCount = new Set(sessions.map((session) => session.project_id)).size
  const revisionCount = sessions.reduce(
    (total, session) => total + session.current_revision,
    0,
  )

  return (
    <div className="cursor-container sessions-overview">
      <header className="page-header page-header--section sessions-overview-header">
        <div>
          <h1 className="cursor-page-title">Sessions</h1>
          <p>Explore agent sessions across every project.</p>
        </div>
      </header>

      <section className="sessions-overview-metrics" aria-label="Session overview">
        <OverviewMetric label="Total sessions" value={NUMBER_FORMATTER.format(sessions.length)} />
        <OverviewMetric label="Active now" value={NUMBER_FORMATTER.format(activeCount)} tone="live" />
        <OverviewMetric label="Projects" value={NUMBER_FORMATTER.format(projectCount)} />
        <OverviewMetric label="Transcript revisions" value={NUMBER_FORMATTER.format(revisionCount)} />
      </section>

      <section className="sessions-overview-list" aria-labelledby="recent-sessions-title">
        <header>
          <div>
            <span>Workspace</span>
            <h2 id="recent-sessions-title">Recent sessions</h2>
          </div>
          <small>Most recently active first</small>
        </header>

        {loading ? (
          <OverviewState>Loading sessions…</OverviewState>
        ) : error === undefined ? (
          sessions.length === 0 ? (
            <OverviewState>No sessions yet.</OverviewState>
          ) : (
            <div className="sessions-overview-table-wrap">
              <table className="sessions-overview-table">
                <thead>
                  <tr>
                    <th scope="col">Session</th>
                    <th scope="col">Project</th>
                    <th scope="col">Harness</th>
                    <th scope="col">Revisions</th>
                    <th scope="col">Status</th>
                    <th scope="col">Last activity</th>
                  </tr>
                </thead>
                <tbody>
                  {sessions.map((session) => (
                    <SessionOverviewRow key={session.id} session={session} />
                  ))}
                </tbody>
              </table>
            </div>
          )
        ) : (
          <OverviewState>
            <span>{error}</span>
            <button type="button" onClick={onRetry}>Try again</button>
          </OverviewState>
        )}
      </section>
    </div>
  )
}

function SessionOverviewRow({ session }: { session: SessionListItem }) {
  return (
    <tr>
      <td>
        <Link to={`/sessions/${encodeURIComponent(session.id)}`}>{session.title}</Link>
        <small>{shortId(session.id)}</small>
      </td>
      <td>{session.project_name}</td>
      <td>{formatHarness(session.harness_id)}</td>
      <td>{NUMBER_FORMATTER.format(session.current_revision)}</td>
      <td><SessionStatus session={session} /></td>
      <td><time dateTime={session.last_activity_at}>{DATE_FORMATTER.format(new Date(session.last_activity_at))}</time></td>
    </tr>
  )
}

function SessionStatus({ session }: { session: SessionListItem }) {
  const label = session.creation_state !== 'accepted'
    ? formatHarness(session.creation_state)
    : session.is_active
      ? 'Active'
      : 'Complete'
  return <span className="sessions-status" data-live={session.is_active}>{label}</span>
}

function OverviewMetric({
  label,
  tone,
  value,
}: {
  label: string
  tone?: 'live'
  value: string
}) {
  return (
    <article className="sessions-overview-metric" data-tone={tone}>
      <span>{label}</span>
      <strong>{value}</strong>
    </article>
  )
}

function SidebarState({ children }: { children: ReactNode }) {
  return <div className="sessions-sidebar-state">{children}</div>
}

function OverviewState({ children }: { children: ReactNode }) {
  return <div className="sessions-overview-state" role="status">{children}</div>
}

function filterSessions(sessions: readonly SessionListItem[], search: string) {
  const query = search.trim().toLocaleLowerCase()
  if (query.length === 0) return sessions
  return sessions.filter((session) =>
    session.title.toLocaleLowerCase().includes(query) ||
    session.project_name.toLocaleLowerCase().includes(query) ||
    session.harness_id.toLocaleLowerCase().includes(query),
  )
}

function formatHarness(value: string) {
  return value.replaceAll(/[-_]/g, ' ').replaceAll(/\b\w/g, (letter) => letter.toUpperCase())
}

function shortId(value: string) {
  return `${value.slice(0, 8)}…${value.slice(-4)}`
}
