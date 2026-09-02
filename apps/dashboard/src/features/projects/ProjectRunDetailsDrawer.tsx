import { MessageResponse } from '@/components/ai-elements/message'
import {
  BrainIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  LoaderCircleIcon,
  XIcon,
} from 'lucide-react'
import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
} from 'react'
import type { AnimationEvent, ReactNode } from 'react'
import { useProjectRunMessages } from './project-session-queries'
import type {
  AssistantContent,
  ProjectRunSummary,
  SessionMessage,
  ToolResultMessage,
} from './project-session-types'

const TOOL_RESULT_PREVIEW_LIMIT = 1_200

type ProjectRunDetailsDrawerProps = {
  projectId: string
  sessionId: string
  run: ProjectRunSummary
  refreshSequence: number | null
  isClosing: boolean
  onClose: () => void
  onClosed: () => void
}

export const ProjectRunDetailsDrawer = memo(function ProjectRunDetailsDrawer({
  projectId,
  sessionId,
  run,
  refreshSequence,
  isClosing,
  onClose,
  onClosed,
}: ProjectRunDetailsDrawerProps) {
  const messages = useProjectRunMessages(projectId, sessionId, run.run_id)
  const scrollRef = useRef<HTMLDivElement>(null)
  const closeButtonRef = useRef<HTMLButtonElement>(null)
  const lastRefreshSequenceRef = useRef<number | null>(refreshSequence)
  const isActive = run.status === 'active' || run.status === 'waiting'
  const messageItems = useMemo(
    () =>
      (messages.data?.pages.flatMap((page) => page.items) ?? [])
        .filter((message) => message.run_id === run.run_id)
        .sort((left, right) => left.revision - right.revision),
    [messages.data, run.run_id],
  )
  const transcriptItems = useMemo(
    () => buildRunTranscript(messageItems, run.final_message_id),
    [messageItems, run.final_message_id],
  )
  const fetchNextPage = messages.fetchNextPage
  const refetch = messages.refetch

  useEffect(() => {
    if (messages.hasNextPage && !messages.isFetchingNextPage) {
      void fetchNextPage()
    }
  }, [fetchNextPage, messages.hasNextPage, messages.isFetchingNextPage])

  useEffect(() => {
    if (
      !isActive ||
      refreshSequence === null ||
      !messages.isSuccess ||
      lastRefreshSequenceRef.current === refreshSequence
    ) {
      return
    }
    lastRefreshSequenceRef.current = refreshSequence
    void refetch()
  }, [isActive, messages.isSuccess, refreshSequence, refetch])

  useEffect(() => {
    lastRefreshSequenceRef.current = null
  }, [run.run_id])

  useEffect(() => {
    closeButtonRef.current?.focus()
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !isClosing) onClose()
    }
    document.addEventListener('keydown', closeOnEscape)
    return () => document.removeEventListener('keydown', closeOnEscape)
  }, [isClosing, onClose])

  const finishCloseAnimation = useCallback(
    (event: AnimationEvent<HTMLElement>) => {
      if (isClosing && event.currentTarget === event.target) onClosed()
    },
    [isClosing, onClosed],
  )

  useLayoutEffect(() => {
    if (!isActive || scrollRef.current === null) return
    scrollRef.current.scrollTop = scrollRef.current.scrollHeight
  }, [isActive, transcriptItems.length])

  useLayoutEffect(() => {
    if (scrollRef.current === null) return
    scrollRef.current.scrollTop = isActive ? scrollRef.current.scrollHeight : 0
  }, [isActive, run.run_id])

  return (
    <aside
      className={`project-run-drawer${isClosing ? ' project-run-drawer--closing' : ''}`}
      aria-label="Run details"
      onAnimationEnd={finishCloseAnimation}
    >
      <header className="project-run-drawer-header">
        <button
          ref={closeButtonRef}
          type="button"
          aria-label="Close run details"
          title="Close"
          disabled={isClosing}
          onClick={onClose}
        >
          <XIcon aria-hidden="true" size={16} />
        </button>
      </header>

      <div
        className="project-run-drawer-content"
        ref={scrollRef}
        aria-live="polite"
      >
        {messages.isPending ? (
          <DrawerState icon={<LoaderCircleIcon className="project-run-drawer-spinner" size={16} />}>
            Loading run messages…
          </DrawerState>
        ) : messages.isError ? (
          <DrawerState icon={<CircleAlertIcon size={16} />}>
            <span>Couldn’t load run messages.</span>
            <button type="button" onClick={() => void refetch()}>
              Retry
            </button>
          </DrawerState>
        ) : transcriptItems.length === 0 ? (
          isActive ? (
            <RunActivityIndicator centered />
          ) : (
            <DrawerState icon={<BrainIcon size={16} />}>
              No intermediate messages were recorded for this run.
            </DrawerState>
          )
        ) : (
          transcriptItems.map((item) => (
            <div className="project-run-transcript-item" key={item.id}>
              {item.kind === 'thinking' ? (
                <ThinkingItem content={item.content} />
              ) : item.kind === 'assistant' ? (
                <AssistantItem content={item.content} />
              ) : (
                <ToolItem
                  argumentsValue={item.arguments}
                  name={item.name}
                  result={item.result}
                />
              )}
            </div>
          ))
        )}
        {isActive && transcriptItems.length > 0 ? (
          <RunActivityIndicator />
        ) : null}
      </div>
    </aside>
  )
})

type RunTranscriptItem =
  | {
      kind: 'thinking'
      id: string
      content: string
    }
  | {
      kind: 'assistant'
      id: string
      content: string
    }
  | {
      kind: 'tool'
      id: string
      name: string
      arguments: Record<string, unknown> | string
      result: ToolResultMessage | null
    }

function buildRunTranscript(
  messages: readonly SessionMessage[],
  finalMessageId: string | null,
): RunTranscriptItem[] {
  const toolResults = new Map<string, ToolResultMessage>()
  for (const message of messages) {
    if (message.message.role === 'tool_result') {
      toolResults.set(message.message.tool_call_id, message.message)
    }
  }

  return messages.flatMap((message) => {
    const body = message.message
    if (body.role !== 'assistant') return []

    return body.content.flatMap((part, index) =>
      transcriptAssistantPart(
        part,
        message,
        index,
        message.session_message_id === finalMessageId,
        toolResults,
      ),
    )
  })
}

function transcriptAssistantPart(
  part: AssistantContent,
  message: SessionMessage,
  index: number,
  isFinalMessage: boolean,
  toolResults: ReadonlyMap<string, ToolResultMessage>,
): RunTranscriptItem[] {
  const id = `${message.session_message_id}:${part.type}:${index}`
  if (part.type === 'thinking') {
    return [{
      kind: 'thinking',
      id,
      content: part.thinking_text,
    }]
  }
  if (part.type === 'tool_call') {
    return [{
      kind: 'tool',
      id,
      name: part.name,
      arguments: part.arguments,
      result: toolResults.get(part.tool_call_id) ?? null,
    }]
  }
  if (isFinalMessage || part.response.content.length === 0) return []
  return [{
    kind: 'assistant',
    id,
    content: part.response.content,
  }]
}

function ThinkingItem({ content }: { content: string }) {
  return (
    <section className="project-run-thinking">
      <div className="project-run-item-label">Thinking</div>
      {content.trim().length === 0 ? null : (
        <MessageResponse className="project-session-markdown project-run-markdown">
          {content}
        </MessageResponse>
      )}
    </section>
  )
}

function AssistantItem({ content }: { content: string }) {
  return (
    <section className="project-run-assistant-step">
      <div className="project-run-item-label">Assistant</div>
      <MessageResponse className="project-session-markdown project-run-markdown">
        {content}
      </MessageResponse>
    </section>
  )
}

function ToolItem({
  argumentsValue,
  name,
  result,
}: {
  argumentsValue: Record<string, unknown> | string
  name: string
  result: ToolResultMessage | null
}) {
  const resultPreview = result === null ? null : formatToolResultPreview(result)
  return (
    <details className="project-run-tool">
      <summary>
        <ChevronRightIcon aria-hidden="true" size={13} />
        <span>{name}</span>
      </summary>
      <div className="project-run-tool-details">
        <section>
          <span>Call</span>
          <pre>{formatStructuredValue(argumentsValue)}</pre>
        </section>
        {resultPreview === null ? null : (
          <section data-status={result?.outcome.status}>
            <span>Result</span>
            <pre className="project-run-tool-result-preview">{resultPreview}</pre>
          </section>
        )}
      </div>
    </details>
  )
}

function DrawerState({
  children,
  icon,
}: {
  children: ReactNode
  icon: ReactNode
}) {
  return (
    <div className="project-run-drawer-state" role="status">
      {icon}
      <span>{children}</span>
    </div>
  )
}

function RunActivityIndicator({ centered = false }: { centered?: boolean }) {
  return (
    <div
      className={`project-run-activity-indicator${centered ? ' project-run-activity-indicator--centered' : ''}`}
      role="status"
      aria-label="Working"
    >
      <span aria-hidden="true" />
      <span aria-hidden="true" />
      <span aria-hidden="true" />
    </div>
  )
}

function formatStructuredValue(value: unknown) {
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

function formatToolResultPreview(message: ToolResultMessage) {
  const parts: string[] = []
  if (message.outcome.status === 'error') {
    parts.push(message.outcome.error.message)
  }
  for (const content of message.content) {
    if (content.type === 'text') {
      if (content.content.trim().length > 0) parts.push(content.content)
    } else {
      parts.push('[Image result]')
    }
  }
  if (parts.length === 0 && message.details !== undefined) {
    parts.push(formatStructuredValue(message.details))
  }
  const value = parts.join('\n\n').trim() || 'Completed with no textual output.'
  if (value.length <= TOOL_RESULT_PREVIEW_LIMIT) return value
  return `${value.slice(0, TOOL_RESULT_PREVIEW_LIMIT).trimEnd()}\n…`
}
