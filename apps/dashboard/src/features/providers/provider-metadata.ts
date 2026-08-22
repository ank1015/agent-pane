import type { ProviderKind } from './provider-queries'

export type ProviderOption = {
  id: ProviderKind
  label: string
}

export const PROVIDER_OPTIONS: ProviderOption[] = [
  { id: 'anthropic', label: 'Anthropic' },
  { id: 'chatgpt', label: 'ChatGPT' },
  { id: 'deepseek', label: 'DeepSeek' },
  { id: 'fireworks', label: 'Fireworks' },
  { id: 'openai', label: 'OpenAI' },
  { id: 'openrouter', label: 'OpenRouter' },
]

export const PROVIDER_LABELS: Record<ProviderKind, string> = {
  anthropic: 'Anthropic',
  chatgpt: 'ChatGPT',
  deepseek: 'DeepSeek',
  fireworks: 'Fireworks',
  openai: 'OpenAI',
  openrouter: 'OpenRouter',
}
