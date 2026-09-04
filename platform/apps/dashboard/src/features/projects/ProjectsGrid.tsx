import { Folder01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { AddProjectDialog } from './AddProjectDialog'
import { useProjects } from './project-queries'
import type { Project } from './project-types'

export function ProjectsGrid() {
  const [adding, setAdding] = useState(false)
  const addRef = useRef<HTMLButtonElement>(null)
  const query = useProjects()
  const close = useCallback(() => {
    setAdding(false)
    addRef.current?.focus()
  }, [])

  return (
    <section className="projects-section" aria-labelledby="projects-list-title" aria-busy={query.isPending && query.isFetching}>
      <div className="projects-section-header">
        <h2 id="projects-list-title">Your Projects</h2>
        <button ref={addRef} type="button" className="cursor-button provider-add-button" onClick={() => setAdding(true)}>Add</button>
      </div>
      {query.isPending && !query.data && <ProjectsState>Loading projects...</ProjectsState>}
      {query.isError && !query.data && <ProjectsState><span>{query.error.message}</span><button type="button" className="providers-retry-button" onClick={() => void query.refetch()}>Retry</button></ProjectsState>}
      {query.isError && query.data && <div className="projects-refresh-warning" role="status"><span>Couldn’t refresh projects. Showing the last saved list.</span><button type="button" className="providers-retry-button" onClick={() => void query.refetch()}>Retry</button></div>}
      {!query.isPending && !query.isError && query.data?.length === 0 && <p className="projects-empty" role="status">No projects added</p>}
      {query.data && query.data.length > 0 && <div className="project-card-grid" aria-live="polite">{query.data.map((project) => <ProjectCard key={project.id} project={project} />)}</div>}
      {adding && <AddProjectDialog onClose={close} />}
    </section>
  )
}

function ProjectCard({ project }: { project: Project }) {
  return (
    <Link to={`/projects/${encodeURIComponent(project.id)}`} className="project-card" aria-label={project.name}>
      <div className="project-card-visual">
        {project.avatar ? <img src={project.avatar} alt="" width="800" height="600" loading="lazy" decoding="async" /> : <span className="project-card-fallback" aria-hidden="true"><HugeiconsIcon icon={Folder01Icon} size={28} color="currentColor" strokeWidth={1.25} /></span>}
      </div>
      <div className="project-card-footer"><h2 title={project.name}>{project.name}</h2></div>
    </Link>
  )
}

function ProjectsState({ children }: { children: ReactNode }) {
  return <div className="projects-state" aria-live="polite">{children}</div>
}
