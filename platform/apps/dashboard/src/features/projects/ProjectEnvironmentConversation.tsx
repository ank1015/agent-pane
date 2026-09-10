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
  CheckIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  CopyIcon,
} from 'lucide-react'
import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import type { RefObject } from 'react'
import type { StickToBottomContext } from 'use-stick-to-bottom'
import type { ProjectConversationItem } from './project-conversation'
import type {
  ImageContent,
  MessageContentPart,
  ProjectRunSummary,
  SessionMessage,
} from './project-session-types'

type ProjectEnvironmentConversationProps = {
  sessionKey: string
  submittedSequence?: number
  items: readonly ProjectConversationItem[]
  isPending: boolean
  isError: boolean
  onRetry: () => void
  onOpenRunDetails: (runId: string) => void
}

type SessionScrollPosition = {
  scrollTop: number
  isAtBottom: boolean
}

const MAX_REMEMBERED_SESSION_SCROLL_POSITIONS = 50
const SESSION_BOTTOM_THRESHOLD_PX = 4
const sessionScrollPositions = new Map<string, SessionScrollPosition>()

function rememberSessionScrollPosition(
  sessionKey: string,
  scrollElement: HTMLElement,
) {
  if (
    !sessionScrollPositions.has(sessionKey) &&
    sessionScrollPositions.size >= MAX_REMEMBERED_SESSION_SCROLL_POSITIONS
  ) {
    const oldestSessionKey = sessionScrollPositions.keys().next().value
    if (oldestSessionKey !== undefined) {
      sessionScrollPositions.delete(oldestSessionKey)
    }
  }

  const distanceFromBottom =
    scrollElement.scrollHeight -
    scrollElement.clientHeight -
    scrollElement.scrollTop
  sessionScrollPositions.set(sessionKey, {
    scrollTop: scrollElement.scrollTop,
    isAtBottom: distanceFromBottom <= SESSION_BOTTOM_THRESHOLD_PX,
  })
}

export const ProjectEnvironmentConversation = memo(function ProjectEnvironmentConversation({
  sessionKey,
  submittedSequence = 0,
  items,
  isPending,
  isError,
  onRetry,
  onOpenRunDetails,
}: ProjectEnvironmentConversationProps) {
  const scrollContextRef = useRef<StickToBottomContext>(null)
  useEffect(() => {
    if (submittedSequence === 0) return
    // Explicit sends return to the latest message; background updates respect
    // a reader who has scrolled up.
    const frame = requestAnimationFrame(() => { void scrollContextRef.current?.scrollToBottom({ animation: 'instant', duration: 200 }) })
    return () => cancelAnimationFrame(frame)
  }, [submittedSequence])

  return (
    <Conversation
      className="project-session-conversation"
      contextRef={scrollContextRef}
      initial="instant"
      resize="instant"
    >
      <ConversationContent
        className="project-session-conversation-content"
        scrollClassName="project-session-conversation-scroll"
      >
        {isPending ? (
          <ConversationState message="Loading conversation…" />
        ) : isError && items.length === 0 ? (
          <ConversationState
            message="Couldn’t load this conversation."
            action="Retry"
            onAction={onRetry}
          />
        ) : items.length === 0 ? (
          <ConversationEmptyState
            className="project-session-empty-state"
            title="No messages yet"
            description="Send a message below to start a run."
          />
        ) : (
          items.map((item) =>
            item.kind === 'message' ? (
              <ConversationMessage
                key={item.id}
                message={item.message}
              />
            ) : item.kind === 'run-progress' ? (
              <RunDuration
                key={item.id}
                run={item.run}
                onOpenRunDetails={onOpenRunDetails}
              />
            ) : (
              <RunErrorMessage key={item.id} run={item.run} />
            ),
          )
        )}
      </ConversationContent>
      <ConversationScrollMemory
        scrollContextRef={scrollContextRef}
        sessionKey={sessionKey}
        isReady={!isPending && !isError}
      />
      <ConversationScrollButton
        className="project-session-scroll-button"
        aria-label="Scroll to latest message"
      />
    </Conversation>
  )
})

function ConversationScrollMemory({
  scrollContextRef,
  sessionKey,
  isReady,
}: {
  scrollContextRef: RefObject<StickToBottomContext | null>
  sessionKey: string
  isReady: boolean
}) {
  useLayoutEffect(() => {
    if (!isReady) return

    let animationFrame = 0
    let positionFrame = 0
    let scrollElement: HTMLElement | null | undefined
    let rememberPosition: (() => void) | undefined
    let cancelRestoration: (() => void) | undefined
    let resizeObserver: ResizeObserver | undefined
    let restorationPending = false

    const restoreAndTrack = () => {
      const scrollContext = scrollContextRef.current
      scrollElement = scrollContext?.scrollRef.current
      if (scrollContext == null || scrollElement == null) {
        animationFrame = requestAnimationFrame(restoreAndTrack)
        return
      }

      const rememberedPosition = sessionScrollPositions.get(sessionKey)
      if (rememberedPosition?.isAtBottom === false) {
        const desiredScrollTop = rememberedPosition.scrollTop
        const contentElement =
          scrollContext.contentRef.current ?? scrollElement.firstElementChild
        restorationPending = true
        scrollContext.stopScroll()

        const finishRestoration = () => {
          restorationPending = false
          resizeObserver?.disconnect()
          resizeObserver = undefined
          rememberSessionScrollPosition(sessionKey, scrollElement!)
        }
        const applyRememberedPosition = () => {
          const maximumScrollTop = Math.max(
            0,
            scrollElement!.scrollHeight - scrollElement!.clientHeight,
          )
          scrollElement!.scrollTop = Math.min(
            desiredScrollTop,
            maximumScrollTop,
          )
          if (maximumScrollTop >= desiredScrollTop) {
            finishRestoration()
          }
        }

        applyRememberedPosition()
        if (restorationPending && contentElement instanceof HTMLElement) {
          resizeObserver = new ResizeObserver(applyRememberedPosition)
          resizeObserver.observe(contentElement)
        }
        cancelRestoration = () => {
          if (restorationPending) finishRestoration()
        }
        scrollElement.addEventListener('wheel', cancelRestoration, {
          passive: true,
        })
        scrollElement.addEventListener('touchstart', cancelRestoration, {
          passive: true,
        })
        scrollElement.addEventListener('pointerdown', cancelRestoration, {
          passive: true,
        })
      } else {
        const maximumScrollTop = Math.max(
          0,
          scrollElement.scrollHeight - scrollElement.clientHeight,
        )
        scrollElement.scrollTop = maximumScrollTop
        rememberSessionScrollPosition(sessionKey, scrollElement)
      }

      rememberPosition = () => {
        if (restorationPending) return
        cancelAnimationFrame(positionFrame)
        positionFrame = requestAnimationFrame(() => {
          if (scrollElement !== undefined && scrollElement !== null) {
            rememberSessionScrollPosition(sessionKey, scrollElement)
          }
        })
      }
      scrollElement.addEventListener('scroll', rememberPosition, {
        passive: true,
      })
    }

    restoreAndTrack()
    return () => {
      cancelAnimationFrame(animationFrame)
      cancelAnimationFrame(positionFrame)
      resizeObserver?.disconnect()
      if (scrollElement != null && rememberPosition !== undefined) {
        scrollElement.removeEventListener('scroll', rememberPosition)
      }
      if (scrollElement != null && cancelRestoration !== undefined) {
        scrollElement.removeEventListener('wheel', cancelRestoration)
        scrollElement.removeEventListener('touchstart', cancelRestoration)
        scrollElement.removeEventListener('pointerdown', cancelRestoration)
      }
    }
  }, [isReady, scrollContextRef, sessionKey])

  return null
}

const ConversationMessage = memo(function ConversationMessage({
  message,
}: {
  message: SessionMessage
}) {
  const body = message.message

  if (body.role === 'user') {
    return (
      <UserConversationMessage body={body} createdAt={message.created_at} deliveryLabel={message.deliveryLabel} />
    )
  }

  if (body.role === 'assistant') {
    const responses = body.content.filter(
      (part) => part.type === 'response',
    )
    if (responses.length === 0) return null
    return (
      <Message className="project-session-message" from="assistant">
        <MessageContent className="project-session-assistant-message">
          {responses.map((part, index) => (
            <MessageResponse
              className="project-session-markdown"
              key={`response:${index}`}
            >
              {part.response.content}
            </MessageResponse>
          ))}
        </MessageContent>
        <ConversationMessageMeta
          createdAt={message.created_at}
          copyText={responses.map((part) => part.response.content).join('\n\n')}
          from="assistant"
        />
      </Message>
    )
  }

  return null
})

const UserConversationMessage = memo(function UserConversationMessage({
  body,
  createdAt,
  deliveryLabel,
}: {
  body: Extract<SessionMessage['message'], { role: 'user' }>
  createdAt: string
  deliveryLabel?: string
}) {
  const copyText = useMemo(
    () =>
      body.content
        .filter((part) => part.type === 'text')
        .map((part) => part.content)
        .join('\n\n'),
    [body.content],
  )

  return (
    <Message className="project-session-message" from="user">
      <MessageContent className="project-session-user-message">
        <ContentParts content={body.content} />
      </MessageContent>
      {deliveryLabel ? <span className="project-session-delivery">{deliveryLabel}</span> : null}
      <ConversationMessageMeta
        createdAt={createdAt}
        copyText={copyText}
        from="user"
      />
    </Message>
  )
})

const ConversationMessageMeta = memo(function ConversationMessageMeta({
  createdAt,
  copyText,
  from,
}: {
  createdAt: string
  copyText: string
  from: 'user' | 'assistant'
}) {
  const [copied, setCopied] = useState(false)
  const resetTimerRef = useRef<number | null>(null)
  const timestamp = useMemo(
    () => formatMessageTimestamp(createdAt),
    [createdAt],
  )

  useEffect(
    () => () => {
      if (resetTimerRef.current !== null) {
        window.clearTimeout(resetTimerRef.current)
      }
    },
    [],
  )

  const copyMessage = useCallback(async () => {
    if (copyText.length === 0) return

    try {
      await navigator.clipboard.writeText(copyText)
      setCopied(true)
      if (resetTimerRef.current !== null) {
        window.clearTimeout(resetTimerRef.current)
      }
      resetTimerRef.current = window.setTimeout(() => {
        setCopied(false)
        resetTimerRef.current = null
      }, 1600)
    } catch {
      // Leave the action unchanged when clipboard access is unavailable.
    }
  }, [copyText])

  return (
    <div className="project-session-message-actions" data-from={from}>
      {timestamp === null ? null : (
        <time dateTime={createdAt} title={timestamp.full}>
          {timestamp.relative}
        </time>
      )}
      {copyText.length === 0 ? null : (
        <button
          type="button"
          aria-label={copied ? 'Message copied' : 'Copy message'}
          title={copied ? 'Copied' : 'Copy message'}
          onClick={copyMessage}
        >
          {copied ? (
            <CheckIcon aria-hidden="true" size={13} />
          ) : (
            <CopyIcon aria-hidden="true" size={13} />
          )}
        </button>
      )}
    </div>
  )
})

function formatMessageTimestamp(value: string) {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return null

  const now = new Date()
  const timeOptions: Intl.DateTimeFormatOptions = {
    hour: 'numeric',
    minute: '2-digit',
  }
  const isToday =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate()

  const startOfWeek = new Date(now)
  const daySinceMonday = (now.getDay() + 6) % 7
  startOfWeek.setDate(now.getDate() - daySinceMonday)
  startOfWeek.setHours(0, 0, 0, 0)

  let relative: string
  if (isToday) {
    relative = new Intl.DateTimeFormat(undefined, timeOptions).format(date)
  } else if (date >= startOfWeek) {
    relative = new Intl.DateTimeFormat(undefined, {
      weekday: 'short',
      ...timeOptions,
    }).format(date)
  } else {
    relative = new Intl.DateTimeFormat(undefined, {
      month: 'short',
      day: 'numeric',
      ...(date.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' }),
      ...timeOptions,
    }).format(date)
  }

  return {
    relative,
    full: new Intl.DateTimeFormat(undefined, {
      dateStyle: 'medium',
      timeStyle: 'short',
    }).format(date),
  }
}

const RunErrorMessage = memo(function RunErrorMessage({
  run,
}: {
  run: ProjectRunSummary
}) {
  return (
    <div className="project-session-run-error" role="alert">
      <CircleAlertIcon aria-hidden="true" size={15} />
      <span>
        <strong>Couldn’t complete the run</strong>
        <span>{runFailureMessage(run.error)}</span>
      </span>
    </div>
  )
})

const RunDuration = memo(function RunDuration({
  run,
  onOpenRunDetails,
}: {
  run: Extract<ProjectConversationItem, { kind: 'run-progress' }>['run']
  onOpenRunDetails: (runId: string) => void
}) {
  const isRunning = (run.status === 'running' || run.status === 'ready') || run.status === 'waiting'
  const [now, setNow] = useState(() => Date.now())

  useEffect(() => {
    if (!isRunning) return
    const timer = window.setInterval(() => setNow(Date.now()), 1_000)
    return () => window.clearInterval(timer)
  }, [isRunning])

  const duration = runDuration(run, isRunning ? now : undefined)

  return (
    <button
      type="button"
      className="project-session-run-duration"
      aria-label={`Open details for run ${run.id}`}
      onClick={() => onOpenRunDetails(run.id)}
    >
      <span>{run.status === 'ready' ? 'Queued' : run.status === 'waiting' ? 'Waiting' : run.status === 'aborted' ? 'Stopped' : isRunning ? 'Working' : 'Worked'} for {duration}</span>
      <ChevronRightIcon aria-hidden="true" size={14} />
    </button>
  )
})

function ContentParts({
  content,
}: {
  content: readonly MessageContentPart[]
}) {
  return content.map((part, index) =>
    part.type === 'text' ? (
      <MessageResponse
        className="project-session-markdown"
        key={`text:${index}`}
      >
        {part.content}
      </MessageResponse>
    ) : part.type === 'audio' ? (
      <audio key={`audio:${index}`} controls preload="none" src={part.audio_url} aria-label="Audio attachment" />
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

function imageSource(image: ImageContent) {
  return image.source.type === 'url'
    ? image.source.url
    : `data:${image.source.mime_type};base64,${image.source.data}`
}

function runDuration(run: ProjectRunSummary, currentTime?: number) {
  const activatedAt = Date.parse(run.started_at ?? run.created_at)
  const createdAt = Date.parse(run.created_at)
  const startedAt = Number.isNaN(activatedAt) ? createdAt : activatedAt
  const finishedAt = run.finished_at === null
    ? currentTime ?? Date.now()
    : Date.parse(run.finished_at)

  if (Number.isNaN(startedAt) || Number.isNaN(finishedAt)) return 'a moment'

  const totalSeconds = Math.max(0, Math.floor((finishedAt - startedAt) / 1_000))
  if (totalSeconds < 60) return `${totalSeconds}s`

  const totalMinutes = Math.floor(totalSeconds / 60)
  if (totalMinutes < 60) return `${totalMinutes}m`

  const totalHours = Math.floor(totalMinutes / 60)
  const minutes = totalMinutes % 60
  if (totalHours < 24) {
    return minutes === 0 ? `${totalHours}h` : `${totalHours}h ${minutes}m`
  }

  const days = Math.floor(totalHours / 24)
  const hours = totalHours % 24
  return hours === 0 ? `${days}d` : `${days}d ${hours}h`
}

function runFailureMessage(failure: unknown) {
  if (typeof failure === 'object' && failure !== null) {
    const message = 'message' in failure ? failure.message : undefined
    if (typeof message === 'string') return message
    const code = 'code' in failure ? failure.code : undefined
    if (typeof code === 'string') return humanizeIdentifier(code)
  }
  return 'The run ended with an error. Please try again.'
}

function humanizeIdentifier(value: string) {
  const leaf = value.split('.').at(-1) ?? value
  return leaf
    .replaceAll(/[_-]+/g, ' ')
    .replace(/^./, (character) => character.toUpperCase())
}
