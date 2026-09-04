export type ProviderKind = 'chatgpt' | 'fireworks' | 'openai'

export type CreateProviderInput = { provider: Exclude<ProviderKind, 'chatgpt'>; name: string; api_key: string }
export type ChatGptLoginStart = { login_id: string; authorization_url: string }
export type ChatGptLoginStatus =
  | { status: 'pending' | 'exchanging' }
  | { status: 'succeeded'; provider_id: string }
  | { status: 'failed'; error: string }

export type ProviderAccount = {
  id: string
  name: string
  provider: ProviderKind
  status: 'enabled' | 'disabled' | 'reauth_required'
  created_at: string
  is_default: boolean
}

export type ProviderCredentialMetadata = {
  version: number
  encryption_key_version: number
  expires_at: string | null
  refreshed_at: string | null
  updated_at: string
}

export type ProviderDetail = {
  id: string
  name: string
  provider: ProviderKind
  config: Record<string, unknown>
  status: ProviderAccount['status']
  runtime_revision: number
  created_at: string
  updated_at: string
  is_default: boolean
  credential: ProviderCredentialMetadata
}

export type ProviderDetailResponse = { provider: ProviderDetail }

export type UsageBreakdown = { input: number; output: number; cache_read: number; cache_write: number }
export type ProviderUsage = {
  account_id: string
  request_count: number
  succeeded_count: number
  failed_count: number
  usage_record_count: number
  tokens: UsageBreakdown
  costs: UsageBreakdown & { total: number }
}
export type ProviderRequest = {
  id: string
  account_id: string
  provider: string
  requested_model: string
  response_model: string | null
  status: string
  started_at: string
  completed_at: string | null
  usage: {
    input: number | null
    output: number | null
    cache_read: number | null
    cache_write: number | null
    cost: { total: number; input: number | null; output: number | null; cache_read: number | null; cache_write: number | null } | null
  } | null
}
export type ProviderRequestPage = { items: ProviderRequest[]; next_cursor: string | null }
