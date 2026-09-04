export type Project = {
  id: string
  name: string
  avatar: string | null
}

export type CreateProjectInput = {
  name: string
  avatar: string | null
}

export type ProjectEnvironment = {
  id: string
  project_id: string
  name: string
  workspace_root: string
  workspace_root_path: string | null
  path: string
  created_at: string
  updated_at: string
} & (
  | { type: 'machine'; machine_id: string; snapshot_id: null }
  | { type: 'sandbox'; machine_id: null; snapshot_id: string }
)
