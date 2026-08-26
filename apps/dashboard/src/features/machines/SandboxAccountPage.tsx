import {
  ArrowLeft01Icon,
  Camera01Icon,
  ComputerIcon,
  Folder03Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type { IconSvgElement } from '@hugeicons/react'
import { Link, NavLink, useLocation } from 'react-router-dom'
import { useMachineInventory } from './machine-queries'
import { SandboxAccountEnvironmentTemplates } from './SandboxAccountEnvironmentTemplates'
import { SandboxAccountSandboxes } from './SandboxAccountSandboxes'
import { SandboxAccountSnapshots } from './SandboxAccountSnapshots'

type AccountTab = {
  end?: boolean
  icon: IconSvgElement
  label: string
  suffix: '' | '/sandboxes' | '/snapshots'
}

const ACCOUNT_TABS: AccountTab[] = [
  {
    end: true,
    icon: Folder03Icon,
    label: 'Environment Templates',
    suffix: '',
  },
  {
    icon: ComputerIcon,
    label: 'Sandboxes',
    suffix: '/sandboxes',
  },
  {
    icon: Camera01Icon,
    label: 'Snapshots',
    suffix: '/snapshots',
  },
]

export function SandboxAccountPage({ accountId }: { accountId: string }) {
  const accountPath = `/machine/accounts/${encodeURIComponent(accountId)}`
  const { pathname } = useLocation()
  const { data: inventory, isPending: isInventoryPending } =
    useMachineInventory()
  const account = inventory?.connector_accounts.find(
    (candidate) => candidate.id === accountId,
  )
  const accountName =
    account?.name ?? (isInventoryPending ? 'Loading…' : accountId)
  const isTemplatesPage =
    pathname === accountPath || pathname === `${accountPath}/`
  const isSandboxesPage =
    pathname === `${accountPath}/sandboxes` ||
    pathname === `${accountPath}/sandboxes/`
  const isSnapshotsPage =
    pathname === `${accountPath}/snapshots` ||
    pathname === `${accountPath}/snapshots/`

  return (
    <div className="cursor-shell machine-detail-shell">
      <aside className="cursor-sidebar dashboard-sidebar machine-detail-sidebar">
        <Link
          className="cursor-nav-item dashboard-nav-item machine-detail-back"
          to="/machines"
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
          aria-label="Sandbox account"
        >
          {ACCOUNT_TABS.map((tab) => (
            <NavLink
              key={tab.label}
              end={tab.end}
              className={({ isActive }) =>
                `cursor-nav-item dashboard-nav-item machine-detail-nav-item${
                  isActive ? ' dashboard-nav-item--active' : ''
                }`
              }
              to={`${accountPath}${tab.suffix}`}
            >
              <span className="nav-icon-frame" aria-hidden="true">
                <HugeiconsIcon
                  className="nav-item-icon"
                  icon={tab.icon}
                  size={16}
                  color="currentColor"
                  strokeWidth={1.5}
                />
              </span>
              <span className="nav-item-label">{tab.label}</span>
            </NavLink>
          ))}
        </nav>
      </aside>

      <main className="cursor-main machine-detail-main">
        {isTemplatesPage ? (
          <SandboxAccountEnvironmentTemplates
            accountId={accountId}
            accountName={accountName}
            isAccountPending={isInventoryPending}
            provider={account?.provider}
          />
        ) : null}
        {isSandboxesPage ? (
          <SandboxAccountSandboxes
            account={account}
            accountId={accountId}
            accountName={accountName}
            isAccountPending={isInventoryPending}
          />
        ) : null}
        {isSnapshotsPage ? (
          <SandboxAccountSnapshots
            accountId={accountId}
            accountName={accountName}
            isAccountPending={isInventoryPending}
          />
        ) : null}
      </main>
    </div>
  )
}
