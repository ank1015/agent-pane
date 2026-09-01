import {
  Conversation,
  ConversationContent,
  ConversationEmptyState,
  ConversationScrollButton,
} from '@/components/ai-elements/conversation'
import {
  Message,
  MessageContent,
  MessageResponse,
} from '@/components/ai-elements/message'
import {
  CheckCircle2Icon,
  ChevronDownIcon,
  CircleAlertIcon,
  CircleStopIcon,
  LoaderCircleIcon,
  WrenchIcon,
} from 'lucide-react'
import { memo, useMemo, useState } from 'react'
import type { ProjectConversationItem } from './project-conversation'
import { useProjectRunEvents } from './project-session-queries'
import type {
  AssistantContent,
  ImageContent,
  RunEvent,
  RunStatus,
  SessionMessage,
  SessionMessageBody,
} from './project-session-types'

type ProjectEnvironmentConversationProps = {
  projectId: string
  sessionId: string
  items: readonly ProjectConversationItem[]
  activeRunId: string | null
  streamState: 'idle' | 'connecting' | 'connected' | 'reconnecting' | 'closed' | 'error'
  isPending: boolean
  isError: boolean
  onRetry: () => void
  onAbortRun: () => void
  isAborting: boolean
}

export function ProjectEnvironmentConversation({
  projectId,
  sessionId,
  items,
  activeRunId,
  streamState,
  isPending,
  isError,
  onRetry,
  onAbortRun,
  isAborting,
}: ProjectEnvironmentConversationProps) {
  return (
    <Conversation className="project-session-conversation">
      <ConversationContent className="project-session-conversation-content">
        {isPending ? (
          <ConversationState message="Loading conversation…" />
        ) : isError ? (
          <ConversationState
            message="Couldn’t load this conversation."
            action="Retry"
            onAction={onRetry}
          />
        ) : items.length === 0 ? (
          <ConversationEmptyState
            className="project-session-empty-state"
            title="No messages yet"
            description="Describe the environment below to start a run."
          />
        ) : (
          items.map((item) =>
            item.kind === 'message' ? (
              <ConversationMessage
                key={item.id}
                message={item.message}
              />
            ) : (
              <RunProgressCard
                key={item.id}
                projectId={projectId}
                sessionId={sessionId}
                run={item.run}
                events={item.events}
                isActive={item.run.run_id === activeRunId}
                streamState={streamState}
                onAbort={onAbortRun}
                isAborting={isAborting}
              />
            ),
          )
        )}
      </ConversationContent>
      <ConversationScrollButton
        className="project-session-scroll-button"
        aria-label="Scroll to latest message"
      />
    </Conversation>
  )
}

const ConversationMessage = memo(function ConversationMessage({
  message,
}: {
  message: SessionMessage
}) {
  const body = message.message

  if (body.role === 'user') {
    return (
      <Message className="project-session-message" from="user">
        <MessageContent className="project-session-user-message">
          <ContentParts content={body.content} />
        </MessageContent>
      </Message>
    )
  }

  if (body.role === 'assistant') {
    return (
      <Message className="project-session-message" from="assistant">
        <MessageContent className="project-session-assistant-message">
          {body.content.map((part, index) => (
            <AssistantPart
              key={assistantPartKey(part, index)}
              part={part}
            />
          ))}
        </MessageContent>
      </Message>
    )
  }

  if (body.role === 'tool_result') {
    return <ToolResultMessage body={body} />
  }

  if (body.role === 'system') {
    return (
      <div className="project-session-system-message" role="note">
        <ContentParts content={readContentParts(body)} />
      </div>
    )
  }

  return null
})

function AssistantPart({ part }: { part: AssistantContent }) {
  if (part.type === 'response') {
    return (
      <MessageResponse className="project-session-markdown">
        {part.response.content}
      </MessageResponse>
    )
  }

  if (part.type === 'thinking') {
    return (
      <details className="project-session-thinking">
        <summary>
          <ChevronDownIcon aria-hidden="true" size={13} />
          Reasoning
        </summary>
        <MessageResponse className="project-session-thinking-content">
          {part.thinking_text}
        </MessageResponse>
      </details>
    )
  }

  return (
    <details className="project-session-tool-call">
      <summary>
        <WrenchIcon aria-hidden="true" size={13} />
        <span>Used {humanizeIdentifier(part.name)}</span>
        <ChevronDownIcon
          className="project-session-disclosure-chevron"
          aria-hidden="true"
          size={13}
        />
      </summary>
      <pre>{formatJson(part.arguments)}</pre>
    </details>
  )
}

function ToolResultMessage({ body }: { body: SessionMessageBody }) {
  const toolName = readString(body, 'tool_name') ?? 'tool'
  const outcome = readRecord(body, 'outcome')
  const isError = outcome?.status === 'error'

  return (
    <details className="project-session-tool-result">
      <summary>
        <WrenchIcon aria-hidden="true" size={13} />
        <span>
          {humanizeIdentifier(toolName)} {isError ? 'failed' : 'completed'}
        </span>
        <ChevronDownIcon
          className="project-session-disclosure-chevron"
          aria-hidden="true"
          size={13}
        />
      </summary>
      <div className="project-session-tool-result-content">
        <ContentParts content={readContentParts(body)} />
      </div>
    </details>
  )
}

const RunProgressCard = memo(
  function RunProgressCard({
    projectId,
    sessionId,
    run,
    events,
    isActive,
    streamState,
    onAbort,
    isAborting,
  }: {
    projectId: string
    sessionId: string
    run: Extract<ProjectConversationItem, { kind: 'run-progress' }>['run']
    events: readonly RunEvent[]
    isActive: boolean
    streamState: ProjectEnvironmentConversationProps['streamState']
    onAbort: () => void
    isAborting: boolean
  }) {
    const [expanded, setExpanded] = useState(isActive)
    const eventHistory = useProjectRunEvents(
      projectId,
      sessionId,
      run.run_id,
      expanded && events.length === 0,
    )
    const persistedEvents = useMemo(
      () => eventHistory.data?.pages.flatMap((page) => page.items) ?? [],
      [eventHistory.data],
    )
    const visibleEvents = events.length > 0 ? events : persistedEvents
    const latestEvent = visibleEvents.at(-1)
    const status = latestEvent?.run_status ?? run.status
    const isLive = status === 'active' || status === 'waiting'
    const summary = runSummary(status, streamState, latestEvent)

    return (
      <section
        className="project-session-run-progress"
        data-status={status}
        aria-label="Environment run progress"
      >
        <details
          open={expanded}
          onToggle={(event) => setExpanded(event.currentTarget.open)}
        >
          <summary className="project-session-run-summary">
            <RunStatusIcon status={status} streamState={streamState} />
            <span className="project-session-run-summary-copy">
              <strong>{summary.title}</strong>
              <span>{summary.detail}</span>
            </span>
            <ChevronDownIcon
              className="project-session-run-chevron"
              aria-hidden="true"
              size={14}
            />
          </summary>
          {isActive && isLive ? (
            <button
              type="button"
              className="project-session-stop-button"
              disabled={isAborting}
              onClick={onAbort}
            >
              <CircleStopIcon aria-hidden="true" size={13} />
              {isAborting ? 'Stopping…' : 'Stop'}
            </button>
          ) : null}
          <ol className="project-session-event-list">
            {visibleEvents.length === 0 ? (
              <li className="project-session-event-row" data-current={isLive}>
                <span className="project-session-event-marker" />
                <span>
                  {eventHistory.isPending && expanded
                    ? 'Loading activity'
                    : 'Preparing run'}
                </span>
              </li>
            ) : (
              visibleEvents.map((event, index) => (
                <RunEventRow
                  key={event.event_id}
                  event={event}
                  isCurrent={isLive && index === visibleEvents.length - 1}
                />
              ))
            )}
          </ol>
        </details>
      </section>
    )
  },
  (previous, next) =>
    previous.projectId === next.projectId &&
    previous.sessionId === next.sessionId &&
    previous.run.run_id === next.run.run_id &&
    previous.run.status === next.run.status &&
    previous.run.state_version === next.run.state_version &&
    previous.isActive === next.isActive &&
    previous.streamState === next.streamState &&
    previous.isAborting === next.isAborting &&
    previous.onAbort === next.onAbort &&
    previous.events.length === next.events.length &&
    previous.events.at(-1)?.event_id === next.events.at(-1)?.event_id,
)

function RunEventRow({
  event,
  isCurrent,
}: {
  event: RunEvent
  isCurrent: boolean
}) {
  const description = describeRunEvent(event)
  return (
    <li className="project-session-event-row" data-current={isCurrent}>
      <span className="project-session-event-marker" />
      <span className="project-session-event-copy">
        <span>{description.label}</span>
        {description.detail === null ? null : (
          <span>{description.detail}</span>
        )}
      </span>
      <time dateTime={event.occurred_at}>{formatEventTime(event.occurred_at)}</time>
    </li>
  )
}

function RunStatusIcon({
  status,
  streamState,
}: {
  status: RunStatus
  streamState: ProjectEnvironmentConversationProps['streamState']
}) {
  if (status === 'completed') {
    return <CheckCircle2Icon aria-hidden="true" size={16} />
  }
  if (status === 'failed' || status === 'aborted') {
    return <CircleAlertIcon aria-hidden="true" size={16} />
  }
  return (
    <LoaderCircleIcon
      className={streamState === 'reconnecting' ? 'is-reconnecting' : ''}
      aria-hidden="true"
      size={16}
    />
  )
}

function ContentParts({
  content,
}: {
  content: readonly (ImageContent & { type: 'image' } | { type: 'text'; content: string })[]
}) {
  return content.map((part, index) =>
    part.type === 'text' ? (
      <MessageResponse
        className="project-session-markdown"
        key={`text:${index}`}
      >
        {part.content}
      </MessageResponse>
    ) : (
      <img
        className="project-session-message-image"
        key={`image:${index}`}
        src={imageSource(part)}
        alt="User attachment"
        loading="lazy"
      />
    ),
  )
}

function ConversationState({
  message,
  action,
  onAction,
}: {
  message: string
  action?: string
  onAction?: () => void
}) {
  return (
    <div className="project-session-conversation-state" role="status">
      <span>{message}</span>
      {action === undefined || onAction === undefined ? null : (
        <button type="button" onClick={onAction}>
          {action}
        </button>
      )}
    </div>
  )
}

function assistantPartKey(part: AssistantContent, index: number) {
  return part.type === 'tool_call'
    ? `tool:${part.tool_call_id}`
    : `${part.type}:${index}`
}

function imageSource(image: ImageContent) {
  return image.source.type === 'url'
    ? image.source.url
    : `data:${image.source.mime_type};base64,${image.source.data}`
}

function readContentParts(body: SessionMessageBody) {
  const content = 'content' in body ? body.content : undefined
  return Array.isArray(content)
    ? content.filter(isContentPart)
    : []
}

function isContentPart(value: unknown): value is
  | { type: 'text'; content: string }
  | ({ type: 'image' } & ImageContent) {
  if (typeof value !== 'object' || value === null || !('type' in value)) {
    return false
  }
  if (value.type === 'text') {
    return 'content' in value && typeof value.content === 'string'
  }
  return value.type === 'image' && 'source' in value
}

function readString(value: object, key: string) {
  const candidate = key in value ? value[key as keyof typeof value] : undefined
  return typeof candidate === 'string' ? candidate : null
}

function readRecord(value: object, key: string): Record<string, unknown> | null {
  const candidate = key in value ? value[key as keyof typeof value] : undefined
  return typeof candidate === 'object' && candidate !== null
    ? (candidate as Record<string, unknown>)
    : null
}

function runSummary(
  status: RunStatus,
  streamState: ProjectEnvironmentConversationProps['streamState'],
  latestEvent: RunEvent | undefined,
) {
  if (status === 'completed') {
    return { title: 'Environment ready', detail: 'Run completed successfully' }
  }
  if (status === 'failed') {
    return { title: 'Environment run failed', detail: eventFailure(latestEvent) }
  }
  if (status === 'aborted') {
    return { title: 'Environment run stopped', detail: 'Stopped before completion' }
  }
  if (status === 'waiting') {
    return { title: 'Waiting for input', detail: 'The run will resume when ready' }
  }
  if (streamState === 'reconnecting' || streamState === 'error') {
    return { title: 'Creating environment', detail: 'Reconnecting to progress…' }
  }
  return { title: 'Creating environment', detail: latestProgressLabel(latestEvent) }
}

function describeRunEvent(event: RunEvent) {
  const details = event.details
  switch (event.type) {
    case 'run_started':
      return { label: 'Run started', detail: null }
    case 'turn_requested':
      return { label: `Turn ${event.turn_number ?? 1} queued`, detail: null }
    case 'turn_started':
      return { label: `Turn ${event.turn_number ?? 1} started`, detail: null }
    case 'turn_ended':
      return {
        label: `Turn ${event.turn_number ?? 1} finished`,
        detail: typeof details?.reason === 'string'
          ? humanizeIdentifier(details.reason)
          : null,
      }
    case 'run_waiting':
      return {
        label: 'Waiting for input',
        detail: typeof details?.kind === 'string'
          ? humanizeIdentifier(details.kind)
          : null,
      }
    case 'run_resumed':
      return { label: 'Run resumed', detail: null }
    case 'run_completed':
      return { label: 'Environment ready', detail: null }
    case 'run_failed':
      return { label: 'Run failed', detail: eventFailure(event) }
    case 'run_aborted':
      return { label: 'Run stopped', detail: null }
    case 'progress':
      return {
        label: typeof details?.name === 'string'
          ? humanizeIdentifier(details.name)
          : 'Working',
        detail: progressDataSummary(details?.data),
      }
  }
}

function latestProgressLabel(event: RunEvent | undefined) {
  return event === undefined ? 'Preparing run…' : describeRunEvent(event).label
}

function eventFailure(event: RunEvent | undefined) {
  const failure = event?.details?.failure
  if (typeof failure === 'object' && failure !== null) {
    const message = 'message' in failure ? failure.message : undefined
    if (typeof message === 'string') return message
    const code = 'code' in failure ? failure.code : undefined
    if (typeof code === 'string') return humanizeIdentifier(code)
  }
  return 'The run ended with an error'
}

function progressDataSummary(value: unknown) {
  if (typeof value !== 'object' || value === null) return null
  const record = value as Record<string, unknown>
  for (const key of ['message', 'description', 'status', 'model']) {
    if (typeof record[key] === 'string') return record[key]
  }
  return null
}

function humanizeIdentifier(value: string) {
  const leaf = value.split('.').at(-1) ?? value
  return leaf
    .replaceAll(/[_-]+/g, ' ')
    .replace(/^./, (character) => character.toUpperCase())
}

function formatEventTime(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime())
    ? ''
    : EVENT_TIME_FORMATTER.format(date)
}

function formatJson(value: unknown) {
  return typeof value === 'string' ? value : JSON.stringify(value, null, 2)
}

const EVENT_TIME_FORMATTER = new Intl.DateTimeFormat(undefined, {
  hour: 'numeric',
  minute: '2-digit',
  second: '2-digit',
})
