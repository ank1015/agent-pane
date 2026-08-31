import { useQuery } from '@tanstack/react-query'
import { getJson } from '../../lib/api-client'
import type { ProviderKind } from '../providers/provider-queries'

export type HarnessSummary = {
  id: string
  name: string
  description: string | null
  created_at: string
  updated_at: string
}

export type HarnessProviderModelOptions = {
  account_id: string
  name: string
  provider: ProviderKind
  model_ids: string[]
}

export type HarnessModelOptions = {
  providers: HarnessProviderModelOptions[]
  reasoning_levels: string[]
}

export const harnessKeys = {
  all: ['harnesses'] as const,
  list: () => [...harnessKeys.all, 'list'] as const,
  modelOptions: (harnessId: string) =>
    [...harnessKeys.all, 'detail', harnessId, 'model-options'] as const,
}

export function useHarnesses() {
  return useQuery({
    queryKey: harnessKeys.list(),
    queryFn: ({ signal }) =>
      getJson<HarnessSummary[]>('/api/harnesses', signal),
  })
}

export function useHarnessModelOptions(harnessId: string) {
  return useQuery({
    queryKey: harnessKeys.modelOptions(harnessId),
    queryFn: ({ signal }) =>
      getJson<HarnessModelOptions>(
        `/api/harnesses/${encodeURIComponent(harnessId)}/model-options`,
        signal,
      ),
    enabled: harnessId.length > 0,
  })
}
