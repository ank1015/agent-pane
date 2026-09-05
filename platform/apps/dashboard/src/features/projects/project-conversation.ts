import type { ProjectRunSummary, RunInput, SessionMessage } from './project-session-types'
import { isLiveRunStatus } from './project-session-types'

export type ProjectConversationItem =
  | { kind: 'message'; id: string; message: SessionMessage }
  | { kind: 'run-progress'; id: string; run: ProjectRunSummary }
  | { kind: 'run-error'; id: string; run: ProjectRunSummary }

// History is authoritative. The input feed fills the gap before a harness appends
// an input, and keeps rejected/unconsumed inputs visible after a terminal run.
export function buildConversation(messages: readonly SessionMessage[], runs: readonly ProjectRunSummary[], inputs: readonly RunInput[]): ProjectConversationItem[] {
  const outerIds = new Set(messages.map(m => m.message_id))
  const runMap = new Map(runs.map(run => [run.id, run]))
  const userIds = new Set(messages.filter(m => m.message.role === 'user').map(m => m.message.id))
  const pending: SessionMessage[] = inputs.flatMap(input => {
    const body = input.payload.message
    if (!body || body.role !== 'user' || userIds.has(body.id) || (input.handling?.message_id && outerIds.has(input.handling.message_id))) return []
    userIds.add(body.id)
    const run = runMap.get(input.run_id)
    const label = input.status === 'rejected' ? 'Not delivered' : input.status === 'pending'
      ? run && !isLiveRunStatus(run.status) ? 'Not consumed by this run' : undefined
      : undefined
    return [{ message_id: 'input:' + input.id, revision: 0, run_id: input.run_id, origin_run_id: input.run_id, message: body, created_at: input.created_at, deliveryLabel: label }]
  })
  const result: ProjectConversationItem[] = []
  const add = (message: SessionMessage) => result.push({ kind: 'message', id: message.message_id, message })
  // Forked history has no local run. Show user messages and complete replies.
  for (const message of messages) {
    if (message.run_id === null && (message.message.role === 'user' ||
      (message.message.role === 'assistant' && !message.message.content.some(p => p.type === 'tool_call')))) add(message)
  }
  const byRun = new Map<string, SessionMessage[]>()
  for (const message of [...messages, ...pending]) {
    if (!message.run_id) continue
    const group = byRun.get(message.run_id) ?? []
    group.push(message)
    byRun.set(message.run_id, group)
  }
  for (const run of runs) {
    const group = byRun.get(run.id) ?? []
    for (const message of group) if (message.message.role === 'user') add(message)
    result.push({ kind: 'run-progress', id: 'progress:' + run.id, run })
    const final = group.find(m => m.message_id === run.final_message_id)
    if (final) add(final)
    if (run.status === 'failed') result.push({ kind: 'run-error', id: 'error:' + run.id, run })
    byRun.delete(run.id)
  }
  // Independent reads can briefly be at different revisions.
  for (const group of byRun.values()) for (const message of group) if (message.message.role === 'user') add(message)
  return result
}
