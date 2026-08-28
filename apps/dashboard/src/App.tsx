import {
  Analytics03Icon,
  ChatBotIcon,
  ComputerIcon,
  Folder01Icon,
  LayoutAlignLeftIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type { IconSvgElement } from '@hugeicons/react'
import {
  Navigate,
  NavLink,
  Route,
  Routes,
  matchPath,
  useLocation,
} from 'react-router-dom'
import { HarnessesTable } from './features/harnesses/HarnessesTable'
import { MachinePage } from './features/machines/MachinePage'
import { MachinesSection } from './features/machines/MachinesSection'
import { SandboxAccountPage } from './features/machines/SandboxAccountPage'
import { ProvidersTable } from './features/providers/ProvidersTable'
import { ProjectPage } from './features/projects/ProjectPage'
import { ProjectsGrid } from './features/projects/ProjectsGrid'
import { useDashboardStore } from './stores/dashboard-store'
import './App.css'

type SectionId = 'projects' | 'harnesses' | 'machines' | 'providers'

type NavigationItem = {
  id: SectionId
  path: `/${SectionId}`
  label: string
  icon: IconSvgElement
}

const NAVIGATION: NavigationItem[] = [
  {
    id: 'projects',
    path: '/projects',
    label: 'Projects',
    icon: Folder01Icon,
  },
  {
    id: 'harnesses',
    path: '/harnesses',
    label: 'Harnesses',
    icon: ChatBotIcon,
  },
  {
    id: 'machines',
    path: '/machines',
    label: 'Machines',
    icon: ComputerIcon,
  },
  {
    id: 'providers',
    path: '/providers',
    label: 'Providers',
    icon: Analytics03Icon,
  },
]

function App() {
  const isSidebarCollapsed = useDashboardStore(
    (state) => state.isSidebarCollapsed,
  )
  const expandSidebar = useDashboardStore((state) => state.expandSidebar)
  const toggleSidebar = useDashboardStore((state) => state.toggleSidebar)
  const { pathname } = useLocation()
  const sandboxAccountMatch =
    matchPath('/machine/accounts/:accountId', pathname) ??
    matchPath('/machine/accounts/:accountId/*', pathname)
  const machineMatch = matchPath('/machines/:machineId', pathname)
  const projectMatch =
    matchPath('/projects/:projectId', pathname) ??
    matchPath('/projects/:projectId/*', pathname)
  const activeItem =
    NAVIGATION.find((item) => item.path === pathname) ?? NAVIGATION[0]

  if (machineMatch !== null) {
    return <MachinePage machineId={machineMatch.params.machineId ?? ''} />
  }

  if (sandboxAccountMatch !== null) {
    return (
      <SandboxAccountPage
        accountId={sandboxAccountMatch.params.accountId ?? ''}
      />
    )
  }

  if (projectMatch !== null) {
    return <ProjectPage projectId={projectMatch.params.projectId ?? ''} />
  }

  return (
    <div
      className="cursor-shell dashboard-shell"
      data-sidebar-collapsed={isSidebarCollapsed}
    >
      <aside
        className="cursor-sidebar dashboard-sidebar"
        onClick={(event) => {
          if (
            isSidebarCollapsed &&
            !(event.target as HTMLElement).closest('button, a')
          ) {
            expandSidebar()
          }
        }}
      >
        <div className="sidebar-header">
          <div className="sidebar-brand" aria-label="Agent Pane">
            <img
              className="brand-mark"
              src="/logo.svg"
              alt=""
              width="20"
              height="18"
            />
          </div>

          <button
            type="button"
            className="cursor-button cursor-button--ghost cursor-icon-button sidebar-toggle"
            aria-label={
              isSidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'
            }
            title={isSidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
            onClick={toggleSidebar}
          >
            <HugeiconsIcon
              icon={LayoutAlignLeftIcon}
              size={16}
              color="currentColor"
              strokeWidth={1.5}
              aria-hidden="true"
            />
          </button>
        </div>

        <nav className="cursor-nav dashboard-nav" aria-label="Dashboard">
          {NAVIGATION.map((item) => {
            return (
              <NavLink
                key={item.id}
                to={item.path}
                className={({ isActive }) =>
                  `cursor-nav-item dashboard-nav-item${
                    isActive ||
                    (item.id === 'machines' && sandboxAccountMatch !== null)
                      ? ' dashboard-nav-item--active'
                      : ''
                  }`
                }
                title={isSidebarCollapsed ? item.label : undefined}
              >
                <span className="nav-icon-frame" aria-hidden="true">
                  <HugeiconsIcon
                    className="nav-item-icon"
                    icon={item.icon}
                    size={16}
                    color="currentColor"
                    strokeWidth={1.5}
                  />
                </span>
                <span className="nav-item-label">{item.label}</span>
              </NavLink>
            )
          })}
        </nav>

        <div className="sidebar-footer" aria-hidden="true">
          <span className="status-dot" />
          <span>Agent Pane</span>
        </div>
      </aside>

      <main className="cursor-main dashboard-main">
        <Routes>
          <Route path="/" element={<Navigate to="/projects" replace />} />
          <Route path="/projects" element={null} />
          <Route path="/projects/:projectId" element={null} />
          <Route path="/projects/:projectId/*" element={null} />
          <Route path="/harnesses" element={null} />
          <Route path="/machines" element={null} />
          <Route path="/providers" element={null} />
          <Route path="*" element={<Navigate to="/projects" replace />} />
        </Routes>

        <div className="cursor-container">
          {activeItem.id !== 'machines' ? (
            <header className="page-header page-header--section">
              <h1 className="cursor-page-title">{activeItem.label}</h1>
            </header>
          ) : null}

          {activeItem.id === 'providers' ? (
            <ProvidersTable />
          ) : activeItem.id === 'machines' ? (
            <MachinesSection />
          ) : activeItem.id === 'harnesses' ? (
            <HarnessesTable />
          ) : activeItem.id === 'projects' ? (
            <ProjectsGrid />
          ) : null}
        </div>
      </main>
    </div>
  )
}

export default App
