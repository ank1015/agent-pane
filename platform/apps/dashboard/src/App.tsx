import { Navigate, Route, Routes } from 'react-router-dom'
import { DashboardShell } from './components/DashboardShell'
import { MachinesPage } from './features/machines/MachinesPage'
import { ProvidersPage } from './features/providers/ProvidersPage'
import { ProviderPage } from './features/providers/ProviderPage'
import { ProjectsPage } from './features/projects/ProjectsPage'

function App() {
  return (
    <Routes>
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
