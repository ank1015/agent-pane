import {
  ArrowLeft01Icon,
  Folder03Icon,
  Globe02Icon,
  ZapIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { Link, NavLink } from 'react-router-dom'

const PROJECT_NAVIGATION = [
  { suffix: '', label: 'Environments', icon: Folder03Icon },
  { suffix: '/triggers', label: 'Triggers', icon: ZapIcon },
  { suffix: '/sites', label: 'Sites', icon: Globe02Icon },
] as const

export function ProjectPage({ projectId }: { projectId: string }) {
  const projectPath = `/projects/${encodeURIComponent(projectId)}`

  return (
    <div className="cursor-shell machine-detail-shell">
      <aside className="cursor-sidebar dashboard-sidebar machine-detail-sidebar">
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

        <nav
          className="cursor-nav dashboard-nav machine-detail-nav"
          aria-label="Project"
        >
          {PROJECT_NAVIGATION.map((item) => (
            <NavLink
              end
              className={({ isActive }) =>
                `cursor-nav-item dashboard-nav-item machine-detail-nav-item${
                  isActive ? ' dashboard-nav-item--active' : ''
                }`
              }
              key={item.label}
              to={`${projectPath}${item.suffix}`}
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
          ))}
        </nav>
      </aside>

      <main className="cursor-main machine-detail-main" />
    </div>
  )
}
