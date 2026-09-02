import type {
  ProjectRunSummary,
  SessionMessage,
} from './project-session-types'

export type ProjectConversationItem =
  | { kind: 'message'; id: string; message: SessionMessage }
  | {
      kind: 'run-progress'
      id: string
      run: ProjectRunSummary
    }
  | {
      kind: 'run-error'
      id: string
      run: ProjectRunSummary
    }

export function buildProjectConversation(
  messages: readonly SessionMessage[],
  runs: readonly ProjectRunSummary[],
): ProjectConversationItem[] {
  const runByTriggerMessage = new Map(
    runs.map((run) => [run.trigger_message_id, run] as const),
  )
  const finalMessageIds = new Set(
    runs.flatMap((run) =>
      run.final_message_id === null ? [] : [run.final_message_id],
    ),
  )
  const orderedMessages = deduplicateById(
    messages,
    (message) => message.session_message_id,
  ).toSorted((left, right) => left.revision - right.revision)
  const items: ProjectConversationItem[] = []

  for (const message of orderedMessages) {
    if (isLegacyCodexEnvironmentMessage(message)) continue
    if (
      message.message.role === 'user' ||
      (message.message.role === 'assistant' &&
        finalMessageIds.has(message.session_message_id))
    ) {
      items.push({
        kind: 'message',
        id: `message:${message.session_message_id}`,
        message,
      })
    }
    const run = runByTriggerMessage.get(message.session_message_id)
    if (run !== undefined) {
      items.push({
        kind: 'run-progress',
        id: `run:${run.run_id}`,
        run,
      })
      if (run.status === 'failed') {
        items.push({
          kind: 'run-error',
          id: `run-error:${run.run_id}`,
          run,
        })
      }
    }
  }

  return items
}

function isLegacyCodexEnvironmentMessage(message: SessionMessage) {
  const body = message.message
  return (
    message.origin === 'harness' &&
    body.role === 'user' &&
    body.id.startsWith('codex-environment-') &&
    body.content.some(
      (part) =>
        part.type === 'text' && part.metadata?.codex_environment !== undefined,
    )
  )
}

function deduplicateById<T>(values: readonly T[], id: (value: T) => string) {
  return [...new Map(values.map((value) => [id(value), value])).values()]
}
