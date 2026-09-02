import type {
  AssistantUsage,
  JsonObject,
  SessionMessage,
} from '../projects/project-session-types'

export type SessionVisualizerStep = {
  message: SessionMessage
  label: string
  cost: number
  cumulativeCost: number
  latencyMs: number
  cumulativeLatencyMs: number
  latencyEstimated: boolean
  inputTokens: number
  outputTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
}

type ToolCallStart = {
  timestamp: number
}

export function buildSessionVisualizerSteps(
  messages: readonly SessionMessage[],
): SessionVisualizerStep[] {
  const ordered = [...new Map(
    messages.map((message) => [message.session_message_id, message]),
  ).values()].toSorted((left, right) => left.revision - right.revision)
  const toolCallStarts = new Map<string, ToolCallStart>()
  let cumulativeCost = 0
  let cumulativeLatencyMs = 0

  return ordered.map((message) => {
    const body = message.message
    if (body.role === 'assistant') {
      for (const part of body.content) {
        if (part.type === 'tool_call') {
          toolCallStarts.set(part.tool_call_id, { timestamp: body.timestamp })
        }
      }
    }

    const usage = usageFromMessage(message)
    const cost = finiteNumber(usage?.cost?.total)
    const inputTokens = finiteNumber(usage?.input)
    const outputTokens = finiteNumber(usage?.output)
    const cacheReadTokens = finiteNumber(usage?.cache_read)
    const cacheWriteTokens = finiteNumber(usage?.cache_write)
    const toolStart = body.role === 'tool_result'
      ? toolCallStarts.get(body.tool_call_id)
      : undefined
    const latencyEstimated = body.role === 'tool_result' && toolStart !== undefined
    const latencyMs = body.role === 'assistant'
      ? finiteNumber(body.duration_ms)
      : latencyEstimated
        ? Math.max(0, body.timestamp - toolStart.timestamp)
        : 0

    cumulativeCost += cost
    cumulativeLatencyMs += latencyMs

    return {
      message,
      label: messageLabel(message),
      cost,
      cumulativeCost,
      latencyMs,
      cumulativeLatencyMs,
      latencyEstimated,
      inputTokens,
      outputTokens,
      cacheReadTokens,
      cacheWriteTokens,
    }
  })
}

export function messageLabel(message: SessionMessage) {
  const body = message.message
  switch (body.role) {
    case 'user':
      return 'User message'
    case 'assistant':
      return body.content.some((part) => part.type === 'tool_call')
        ? 'Assistant · tool request'
        : 'Assistant response'
    case 'tool_result':
      return `Tool result · ${body.tool_name}`
    case 'system':
      return 'System message'
    case 'custom':
      return body.tag === undefined ? 'Custom message' : `Custom · ${body.tag}`
  }
}

export function usageFromMessage(message: SessionMessage): AssistantUsage | undefined {
  if (message.message.role === 'assistant') {
    return message.message.usage
  }
  if (message.message.role !== 'custom') {
    return undefined
  }
  return parseUsage(message.message.content.usage)
}

export function textFromMessage(message: SessionMessage) {
  const body = message.message
  switch (body.role) {
    case 'user':
      return body.content
        .flatMap((part) => part.type === 'text' ? [part.content] : ['[Image]'])
        .join('\n')
    case 'assistant':
      return body.content
        .flatMap((part) => {
          if (part.type === 'response') return [part.response.content]
          if (part.type === 'thinking') return [part.thinking_text]
          return [`${part.name}(${formatInline(part.arguments)})`]
        })
        .filter((value) => value.length > 0)
        .join('\n')
    case 'system':
      return body.content.map((part) => part.content).join('\n')
    case 'tool_result':
      return body.content
        .flatMap((part) => part.type === 'text' ? [part.content] : ['[Image]'])
        .join('\n')
    case 'custom':
      return JSON.stringify(body.content, null, 2)
  }
}

function parseUsage(value: unknown): AssistantUsage | undefined {
  if (!isObject(value)) return undefined
  const costValue = isObject(value.cost) ? value.cost : undefined
  const cost = costValue === undefined
    ? undefined
    : {
        input: optionalFiniteNumber(costValue.input),
        output: optionalFiniteNumber(costValue.output),
        cache_read: optionalFiniteNumber(costValue.cache_read),
        cache_write: optionalFiniteNumber(costValue.cache_write),
        total: finiteNumber(costValue.total),
      }
  return {
    input: optionalFiniteNumber(value.input),
    output: optionalFiniteNumber(value.output),
    cache_read: optionalFiniteNumber(value.cache_read),
    cache_write: optionalFiniteNumber(value.cache_write),
    cost,
  }
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function optionalFiniteNumber(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
    ? value
    : undefined
}

function finiteNumber(value: unknown) {
  return optionalFiniteNumber(value) ?? 0
}

function formatInline(value: JsonObject | string) {
  if (typeof value === 'string') return value
  const serialized = JSON.stringify(value)
  return serialized.length > 120 ? `${serialized.slice(0, 117)}…` : serialized
}
