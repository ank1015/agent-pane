import { Link } from 'react-router-dom'

const MOCK_PROJECTS = [
  { id: 'atlas', name: 'Atlas', image: '/project-covers/prism.jpg' },
  {
    id: 'northstar',
    name: 'Northstar',
    image: '/project-covers/northstar.jpg',
  },
  { id: 'pulse', name: 'Pulse', image: '/project-covers/pulse.jpg' },
  { id: 'relay', name: 'Relay', image: '/project-covers/relay.jpg' },
  { id: 'studio', name: 'Studio', image: '/project-covers/studio.jpg' },
  {
    id: 'workbench',
    name: 'Workbench',
    image: '/project-covers/workbench.jpg',
  },
  { id: 'orbit', name: 'Orbit', image: '/project-covers/orbit.jpg' },
  { id: 'atelier', name: 'Atelier', image: '/project-covers/atelier.jpg' },
] as const

export function ProjectsGrid() {
  return (
    <section className="project-card-grid" aria-label="Projects">
      {MOCK_PROJECTS.map((project) => (
        <Link
          className="project-card"
          key={project.id}
          to={`/projects/${encodeURIComponent(project.id)}`}
          aria-label={project.name}
        >
          <div className="project-card-visual">
            <img
              src={project.image}
              alt=""
              width="800"
              height="600"
              loading="lazy"
              decoding="async"
            />
          </div>
          <div className="project-card-footer">
            <h2 title={project.name}>{project.name}</h2>
          </div>
        </Link>
      ))}
    </section>
  )
}
