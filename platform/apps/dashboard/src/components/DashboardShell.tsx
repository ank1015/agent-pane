import {
  Analytics03Icon,
  ComputerIcon,
  Folder01Icon,
  LayoutAlignLeftIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { NavLink, Outlet } from 'react-router-dom'
import { useDashboardStore } from '../stores/dashboard-store'

export function DashboardShell() {
  const isSidebarCollapsed = useDashboardStore(
    (state) => state.isSidebarCollapsed,
  )
  const expandSidebar = useDashboardStore((state) => state.expandSidebar)
  const toggleSidebar = useDashboardStore((state) => state.toggleSidebar)

  return (
    <div
      className="dashboard-shell"
      data-sidebar-collapsed={isSidebarCollapsed}
    >
      <aside
        className="dashboard-sidebar"
        aria-label="Dashboard sidebar"
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
            className="sidebar-toggle"
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

        <nav className="sidebar-nav" aria-label="Dashboard">
          <NavLink
            to="/projects"
            className={({ isActive }) =>
              `sidebar-nav-item${isActive ? ' sidebar-nav-item--active' : ''}`
            }
            title={isSidebarCollapsed ? 'Projects' : undefined}
          >
            <span className="nav-icon-frame" aria-hidden="true">
              <HugeiconsIcon
                icon={Folder01Icon}
                size={16}
                color="currentColor"
                strokeWidth={1.5}
              />
            </span>
            <span className="nav-item-label">Projects</span>
          </NavLink>
          <NavLink
            to="/machines"
            className={({ isActive }) =>
              `sidebar-nav-item${isActive ? ' sidebar-nav-item--active' : ''}`
            }
            title={isSidebarCollapsed ? 'Machines' : undefined}
          >
            <span className="nav-icon-frame" aria-hidden="true">
              <HugeiconsIcon
                icon={ComputerIcon}
                size={16}
                color="currentColor"
                strokeWidth={1.5}
              />
            </span>
            <span className="nav-item-label">Machines</span>
          </NavLink>
          <NavLink
            to="/providers"
            className={({ isActive }) =>
              `sidebar-nav-item${isActive ? ' sidebar-nav-item--active' : ''}`
            }
            title={isSidebarCollapsed ? 'Providers' : undefined}
          >
            <span className="nav-icon-frame" aria-hidden="true">
              <HugeiconsIcon
                icon={Analytics03Icon}
                size={16}
                color="currentColor"
                strokeWidth={1.5}
              />
            </span>
            <span className="nav-item-label">Providers</span>
          </NavLink>
        </nav>
      </aside>

      <main className="dashboard-content">
        <Outlet />
      </main>
    </div>
  )
}
