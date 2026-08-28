import { useQuery } from '@tanstack/react-query'
import { getJson } from '../../lib/api-client'

export type HarnessSummary = {
  id: string
  name: string
  description: string | null
  created_at: string
  updated_at: string
}

export const harnessKeys = {
  all: ['harnesses'] as const,
  list: () => [...harnessKeys.all, 'list'] as const,
}

export function useHarnesses() {
  return useQuery({
    queryKey: harnessKeys.list(),
    queryFn: ({ signal }) =>
      getJson<HarnessSummary[]>('/api/harnesses', signal),
  })
}
