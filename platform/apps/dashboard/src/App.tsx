import { Navigate, Route, Routes } from 'react-router-dom'
import { DashboardShell } from './components/DashboardShell'
import { MachinesPage } from './features/machines/MachinesPage'
import { ProvidersPage } from './features/providers/ProvidersPage'
import { ProviderPage } from './features/providers/ProviderPage'
import { ProjectsPage } from './features/projects/ProjectsPage'
import { ProjectPage, ProjectEnvironmentsPage, ProjectSettingsPage } from './features/projects/ProjectPage'
import { ProjectNewChatPage } from './features/projects/ProjectNewChatPage'
import { ProjectSitesPage } from './features/sites/ProjectSitesPage'
import { SiteViewerPage } from './features/sites/SiteViewerPage'
import { lazy, Suspense } from 'react'

const ProjectSessionPage = lazy(() => import('./features/projects/ProjectSessionPage'))

function App() {
  return (
    <Routes>
      <Route path="/projects/:projectId/sites/:siteId/view" element={<SiteViewerPage />} />
      <Route path="/projects/:projectId" element={<ProjectPage />}>
        <Route index element={<ProjectNewChatPage />} />
        <Route path="environments" element={<ProjectEnvironmentsPage />} />
        <Route path="sites" element={<ProjectSitesPage />} />
        <Route path="settings" element={<ProjectSettingsPage />} />
        <Route path="environments/edit" element={<Navigate to=".." replace />} />
        <Route path="environments/list" element={<Navigate to="../environments" replace />} />
        <Route path=":sessionId" element={<Suspense fallback={<span className="project-chat-loading" role="status" aria-label="Loading chat" />}><ProjectSessionPage /></Suspense>} />
      </Route>
      <Route path="/providers/:providerId" element={<ProviderPage />} />
      <Route path="/" element={<DashboardShell />}>
        <Route index element={<Navigate to="/machines" replace />} />
        <Route path="projects" element={<ProjectsPage />} />
        <Route path="machines" element={<MachinesPage />} />
        <Route path="providers" element={<ProvidersPage />} />
        <Route path="*" element={<Navigate to="/machines" replace />} />
      </Route>
    </Routes>
  )
}

export default App
