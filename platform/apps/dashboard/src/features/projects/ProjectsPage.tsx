import { ProjectsGrid } from './ProjectsGrid'

export function ProjectsPage() {
  return (
    <div className="projects-page">
      <header className="projects-page-header">
        <h1 className="cursor-page-title">Projects</h1>
      </header>
      <ProjectsGrid />
    </div>
  )
}
