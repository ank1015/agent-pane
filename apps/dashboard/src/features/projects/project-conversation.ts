import type {
  ProjectRunSummary,
  RunEvent,
  SessionMessage,
} from './project-session-types'

export type ProjectConversationItem =
  | { kind: 'message'; id: string; message: SessionMessage }
  | {
      kind: 'run-progress'
      id: string
      run: ProjectRunSummary
      events: readonly RunEvent[]
    }

export function buildProjectConversation(
  messages: readonly SessionMessage[],
  runs: readonly ProjectRunSummary[],
  eventsByRun: ReadonlyMap<string, readonly RunEvent[]>,
): ProjectConversationItem[] {
  const runByTriggerMessage = new Map(
    runs.map((run) => [run.trigger_message_id, run] as const),
  )
  const orderedMessages = deduplicateById(
    messages,
    (message) => message.session_message_id,
  ).toSorted((left, right) => left.revision - right.revision)
  const items: ProjectConversationItem[] = []

  for (const message of orderedMessages) {
    items.push({
      kind: 'message',
      id: `message:${message.session_message_id}`,
      message,
    })
    const run = runByTriggerMessage.get(message.session_message_id)
    if (run !== undefined) {
      items.push({
        kind: 'run-progress',
        id: `run:${run.run_id}`,
        run,
        events: deduplicateById(
          eventsByRun.get(run.run_id) ?? [],
          (event) => event.event_id,
        ).toSorted((left, right) => left.sequence - right.sequence),
      })
    }
  }

  return items
}

function deduplicateById<T>(values: readonly T[], id: (value: T) => string) {
  return [...new Map(values.map((value) => [id(value), value])).values()]
}
