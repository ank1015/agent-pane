export type Project = {
  id: string
  name: string
  avatar: string | null
}

export type CreateProjectInput = {
  name: string
  avatar: string | null
}
