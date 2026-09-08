import type { ProjectRunSummary, SessionMessage } from './project-session-types'

export type SessionAnalysisPoint = {
  messageNumber: number
  value: number
}

export type SessionAnalysis = {
  latency: SessionAnalysisPoint[]
  cumulativeCost: SessionAnalysisPoint[]
  totalTimeMs: number
  totalCost: number | undefined
  inputTokens: number | undefined
  outputTokens: number | undefined
  cachedTokens: number | undefined
  totalTokens: number | undefined
  cachePercent: number | undefined
  messageCount: number
  assistantMessageCount: number
  toolCallCount: number
}

export function buildSessionAnalysis(
  messages: readonly SessionMessage[],
  runs: readonly ProjectRunSummary[],
  now = Date.now(),
): SessionAnalysis {
  const ordered = [...messages].sort((left, right) => left.revision - right.revision)
  const latency: SessionAnalysisPoint[] = []
  const cumulativeCost: SessionAnalysisPoint[] = []
  let totalCost = 0
  let inputTokens = 0
  let outputTokens = 0
  let cachedTokens = 0
  let cacheWriteTokens = 0
  let hasCost = false
  let hasInput = false
  let hasOutput = false
  let hasCached = false
  let hasAnyTokens = false
  let assistantMessageCount = 0
  let toolCallCount = 0

  for (const message of ordered) {
    if (message.message.role !== 'assistant') continue
    assistantMessageCount += 1
    const body = message.message
    const duration = finite(body.duration_ms)
    latency.push({ messageNumber: message.revision, value: duration })
    toolCallCount += body.content.filter(part => part.type === 'tool_call').length

    const usage = body.usage
    if (usage?.cost !== undefined) {
      hasCost = true
      totalCost += finite(usage.cost.total)
    }
    if (usage?.input !== undefined) {
      hasInput = true
      hasAnyTokens = true
      inputTokens += finite(usage.input)
    }
    if (usage?.output !== undefined) {
      hasOutput = true
      hasAnyTokens = true
      outputTokens += finite(usage.output)
    }
    if (usage?.cache_read !== undefined) {
      hasCached = true
      hasAnyTokens = true
      cachedTokens += finite(usage.cache_read)
    }
    if (usage?.cache_write !== undefined) {
      hasAnyTokens = true
      cacheWriteTokens += finite(usage.cache_write)
    }
    cumulativeCost.push({ messageNumber: message.revision, value: totalCost })
  }

  const eligibleInput = inputTokens + cachedTokens
  return {
    latency,
    cumulativeCost: hasCost ? cumulativeCost : [],
    totalTimeMs: totalRunTime(runs, ordered, now),
    totalCost: hasCost ? totalCost : undefined,
    inputTokens: hasInput ? inputTokens : undefined,
    outputTokens: hasOutput ? outputTokens : undefined,
    cachedTokens: hasCached ? cachedTokens : undefined,
    totalTokens: hasAnyTokens ? inputTokens + outputTokens + cachedTokens + cacheWriteTokens : undefined,
    cachePercent: hasInput || hasCached ? (eligibleInput === 0 ? 0 : cachedTokens / eligibleInput * 100) : undefined,
    messageCount: ordered.length,
    assistantMessageCount,
    toolCallCount,
  }
}

/** Limit both usage and run time to the message revealed by replay. */
export function buildSessionReplayAnalysis(messages: readonly SessionMessage[], runs: readonly ProjectRunSummary[], index: number): SessionAnalysis {
  const visible = [...messages].sort((left, right) => left.revision - right.revision).slice(0, Math.max(0, index + 1))
  const cutoff = parseTime(visible.at(-1)?.created_at ?? '')
  const start = parseTime(visible[0]?.created_at ?? '')
  const visibleRunIds = new Set(visible.map(message => message.run_id))
  const clippedRuns = cutoff === undefined ? [] : runs.filter(run => visibleRunIds.has(run.id)).map(run => ({
    ...run,
    started_at: new Date(Math.max(start ?? 0, parseTime(run.started_at ?? run.created_at) ?? cutoff)).toISOString(),
    finished_at: new Date(Math.min(cutoff, parseTime(run.finished_at ?? '') ?? cutoff)).toISOString(),
  }))
  const analysis = buildSessionAnalysis(visible, clippedRuns)
  if (analysis.assistantMessageCount === 0) {
    return { ...analysis, totalCost: 0, inputTokens: 0, outputTokens: 0, cachedTokens: 0, totalTokens: 0 }
  }
  return analysis
}

function totalRunTime(runs: readonly ProjectRunSummary[], messages: readonly SessionMessage[], now: number) {
  if (runs.length > 0) {
    return runs.reduce((total, run) => {
      const start = parseTime(run.started_at ?? run.created_at)
      const finish = run.finished_at === null ? now : parseTime(run.finished_at)
      return total + (start === undefined || finish === undefined ? 0 : Math.max(0, finish - start))
    }, 0)
  }
  const times = messages.map(message => parseTime(message.created_at)).filter((value): value is number => value !== undefined)
  return times.length < 2 ? 0 : Math.max(...times) - Math.min(...times)
}

function finite(value: number) {
  return Number.isFinite(value) && value >= 0 ? value : 0
}

function parseTime(value: string) {
  const parsed = Date.parse(value)
  return Number.isNaN(parsed) ? undefined : parsed
}
