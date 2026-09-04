import type { ProviderKind } from './provider-types'

export const PROVIDER_LABELS: Record<ProviderKind, string> = { openai: 'OpenAI', chatgpt: 'ChatGPT', fireworks: 'Fireworks' }
export const PROVIDER_OPTIONS: ProviderKind[] = ['openai', 'chatgpt', 'fireworks']
