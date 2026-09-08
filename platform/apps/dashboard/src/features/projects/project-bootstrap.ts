import type { ProviderAccount } from '../providers/provider-types'
import type { ProjectEnvironment } from './project-types'

export type Harness = {
  id: string
  name: string
  enabled: boolean
  supported_models: Record<string, string[]>
  default_config: Record<string, unknown>
  config_schema: { properties?: Record<string, { type?: string; enum?: unknown[]; default?: unknown }> } | null
}
export type ProjectBootstrap = {
  harnesses: Harness[]
  provider_accounts: ProviderAccount[]
  project_environments: ProjectEnvironment[]
}

export const ENVIRONMENTS_HARNESS_ID = 'environments'
export const SITES_HARNESS_ID = 'sites'

export function harnessOptions(data: ProjectBootstrap | undefined, harness: Harness | undefined) {
  const properties = harness?.config_schema?.properties
  const reasoningLevels = (properties?.reasoning_level?.enum ?? []).filter((value): value is string => typeof value === 'string')
  const defaultReasoning = harness?.default_config.reasoning_level
  const webSearchSupported = properties?.web_search_enabled?.type === 'boolean'
  const defaultWebSearch = harness?.default_config.web_search_enabled ?? properties?.web_search_enabled?.default ?? false
  return {
    accounts: (data?.provider_accounts ?? []).filter(account => account.status === 'enabled').flatMap(account => {
      const models = harness?.supported_models[account.provider] ?? []
      return models.length ? [{ account_id: account.id, name: account.name, provider: account.provider, model_ids: models }] : []
    }),
    reasoningLevels,
    defaultReasoning: typeof defaultReasoning === 'string' && reasoningLevels.includes(defaultReasoning) ? defaultReasoning : reasoningLevels[0],
    webSearchSupported,
    defaultWebSearch: webSearchSupported && defaultWebSearch === true,
  }
}
