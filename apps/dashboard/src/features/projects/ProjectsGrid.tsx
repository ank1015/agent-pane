import { Folder01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useState } from 'react'
import type { ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { AddProjectDialog } from './AddProjectDialog'
import { type Project, useProjects } from './project-queries'

export function ProjectsGrid() {
  const [isAddDialogOpen, setIsAddDialogOpen] = useState(false)
  const { data, error, isError, isPending, refetch } = useProjects()
  const openAddDialog = useCallback(() => setIsAddDialogOpen(true), [])
  const closeAddDialog = useCallback(() => setIsAddDialogOpen(false), [])

  return (
    <section
      className="projects-section"
      aria-busy={isPending}
      aria-labelledby="projects-list-title"
    >
      <div className="projects-section-header">
        <h2 id="projects-list-title">Your Projects</h2>
        <button
          type="button"
          className="cursor-button provider-add-button"
          onClick={openAddDialog}
        >
          Add
        </button>
      </div>

      {isPending ? (
        <ProjectsState>Loading projects...</ProjectsState>
      ) : null}
      {isError ? (
        <ProjectsState>
          <span>{error.message}</span>
          <button
            type="button"
            className="cursor-button cursor-button--ghost"
            onClick={() => void refetch()}
          >
            Retry
          </button>
        </ProjectsState>
      ) : null}
      {!isPending && !isError && data?.length === 0 ? (
        <ProjectsState>No projects added</ProjectsState>
      ) : null}
      {!isPending && !isError && data !== undefined && data.length > 0 ? (
        <div className="project-card-grid" aria-live="polite">
          {data.map((project) => (
            <ProjectCard key={project.id} project={project} />
          ))}
        </div>
      ) : null}

      <AddProjectDialog open={isAddDialogOpen} onClose={closeAddDialog} />
    </section>
  )
}

function ProjectCard({ project }: { project: Project }) {
  return (
    <Link
      className="project-card"
      to={`/projects/${encodeURIComponent(project.id)}`}
      aria-label={project.name}
    >
      <div className="project-card-visual">
        {project.avatar !== null ? (
          <img
            src={project.avatar}
            alt=""
            width="800"
            height="600"
            loading="lazy"
            decoding="async"
          />
        ) : (
          <span className="project-card-fallback" aria-hidden="true">
            <HugeiconsIcon
              icon={Folder01Icon}
              size={28}
              color="currentColor"
              strokeWidth={1.25}
            />
          </span>
        )}
      </div>
      <div className="project-card-footer">
        <h2 title={project.name}>{project.name}</h2>
      </div>
    </Link>
  )
}

function ProjectsState({ children }: { children: ReactNode }) {
  return (
    <div className="projects-state" aria-live="polite">
      {children}
    </div>
  )
}
