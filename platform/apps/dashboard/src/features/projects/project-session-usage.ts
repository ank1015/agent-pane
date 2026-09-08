import type { SessionMessage } from './project-session-types'

export type SessionUsageSummary = {
  cost: string
  cache: string
  context: string
}

export function summarizeSessionUsage(messages: readonly SessionMessage[]): SessionUsageSummary {
  let cost = 0
  let input = 0
  let cached = 0
  let latestContext: number | undefined
  let hasCost = false
  let hasTokens = false

  const finite = (value: number | undefined) => value !== undefined && Number.isFinite(value) && value >= 0 ? value : 0
  for (const message of messages) {
    if (message.message.role !== 'assistant' || !message.message.usage) continue
    const usage = message.message.usage
    const hasMessageTokens = usage.input !== undefined || usage.output !== undefined || usage.cache_read !== undefined || usage.cache_write !== undefined
    hasCost ||= usage.cost !== undefined
    hasTokens ||= usage.input !== undefined || usage.cache_read !== undefined
    cost += finite(usage.cost?.total)
    input += finite(usage.input)
    cached += finite(usage.cache_read)
    if (hasMessageTokens) latestContext = finite(usage.input) + finite(usage.output) + finite(usage.cache_read) + finite(usage.cache_write)
  }

  return {
    cost: hasCost ? '$' + cost.toFixed(5) : '—',
    cache: hasTokens ? (input + cached ? cached / (input + cached) * 100 : 0).toFixed(1) + '%' : '—',
    context: latestContext === undefined ? '—' : (latestContext / 1_000).toFixed(1) + 'k',
  }
}
