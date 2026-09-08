import { Message, MessageContent, MessageResponse } from '@/components/ai-elements/message'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { LoaderCircleIcon, StepBackIcon, StepForwardIcon, WrenchIcon } from 'lucide-react'
import { Fragment, memo, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useParams } from 'react-router-dom'
import { chatMessagesOptions, chatRunsOptions, validId } from './chat-queries'
import { useChatSession } from './chat-queries'
import { buildSessionAnalysis, buildSessionReplayAnalysis, type SessionAnalysisPoint } from './session-analysis'
import type { AssistantMessage, ImageContent, SessionMessage, ToolResultMessage } from './project-session-types'

const EMPTY_MESSAGES: SessionMessage[] = []

export default function ProjectSessionAnalyzerPage() {
  const { projectId = '', sessionId = '' } = useParams()
  if (!validId(projectId) || !validId(sessionId)) return <p className="session-analyzer-state" role="alert">Invalid session URL.</p>
  return <AnalyzerView key={sessionId.toLowerCase()} projectId={projectId.toLowerCase()} sessionId={sessionId.toLowerCase()} />
}

function AnalyzerView({ projectId, sessionId }: { projectId: string; sessionId: string }) {
  const client = useQueryClient()
  const session = useChatSession(sessionId)
  const live = Boolean(session.data?.active_run)
  const enabled = session.data?.project_id === projectId
  const messages = useQuery({ ...chatMessagesOptions(client, sessionId, live), enabled })
  const runs = useQuery({ ...chatRunsOptions(sessionId, live), enabled })
  const history = messages.data ?? EMPTY_MESSAGES
  const ordered = useMemo(() => [...history].sort((left, right) => left.revision - right.revision), [history])
  const [replayIndex, setReplayIndex] = useState<number | null>(null)
  const replaying = replayIndex !== null
  const currentIndex = replayIndex === null ? null : Math.min(replayIndex, ordered.length - 1)
  const currentMessage = currentIndex === null ? undefined : ordered[currentIndex]
  const transcriptRef = useRef<HTMLElement>(null)
  const analysis = useMemo(() => currentIndex === null
    ? buildSessionAnalysis(ordered, runs.data ?? [])
    : buildSessionReplayAnalysis(ordered, runs.data ?? [], currentIndex), [ordered, runs.data, currentIndex])
  useLayoutEffect(() => {
    if (currentIndex === null) return
    const transcript = transcriptRef.current
    const target = transcript?.querySelector<HTMLElement>(`[data-message-index="${currentIndex}"]`)
    if (!transcript || !target) return
    const viewport = transcript.getBoundingClientRect()
    const message = target.getBoundingClientRect()
    if (message.top < viewport.top + 16 || message.top > viewport.bottom - 64) {
      transcript.scrollTo({ top: transcript.scrollTop + message.top - viewport.top - 32, behavior: 'smooth' })
    }
  }, [currentIndex])
  const retry = () => { void Promise.all([session.refetch(), messages.refetch(), runs.refetch()]) }

  if (!session.data) {
    return <AnalyzerState loading={session.isPending} message={session.isError ? session.error.message : 'Loading session analysis…'} onRetry={session.isError ? retry : undefined} />
  }
  if (!enabled) return <AnalyzerState message="This session does not belong to this project." />

  const readError = messages.isError || runs.isError
  return (
    <div className={`session-analyzer-page${replaying ? ' is-replaying' : ''}`}>
      <header className="session-analyzer-header">
        <div className="session-analyzer-heading">
          <h1 title={session.data.title ?? 'Untitled session'}>{session.data.title ?? 'Untitled session'}</h1>
        </div>
        <button type="button" className="session-analyzer-replay" disabled={ordered.length === 0} aria-pressed={replaying} onClick={() => setReplayIndex(replaying ? null : 0)}>{replaying ? 'Stop' : 'Replay'}</button>
      </header>
      <main className="session-analyzer-layout">
        <section ref={transcriptRef} className="session-analyzer-transcript" aria-label="Full session transcript">
          {readError ? <div className="session-analyzer-notice" role="status">Couldn’t refresh all session data. <button type="button" onClick={retry}>Retry</button></div> : null}
          {messages.isPending ? <AnalyzerState loading message="Loading transcript…" /> : history.length === 0 ? <AnalyzerState message="No committed messages yet." /> : <AnalyzerTranscript messages={ordered} currentIndex={currentIndex} />}
        </section>
        <aside className="session-analyzer-sidebar" aria-label="Session analytics">
          <div className="session-analyzer-sidebar-content">
          {!replaying ? <>
          <AnalysisChart title="Latency by assistant message" points={analysis.latency} formatValue={formatLatency} />
          <AnalysisChart title="Cumulative cost" points={analysis.cumulativeCost} formatValue={formatChartCost} />
          </> : null}
          <section className="session-analyzer-stats" aria-labelledby="session-analyzer-stats-title">
            <h2 id="session-analyzer-stats-title">{replaying ? 'Cumulative statistics' : 'Session statistics'}</h2>
            <dl>
              <Stat label="Total time" value={formatDuration(analysis.totalTimeMs)} />
              <Stat label="Total cost" value={formatCost(analysis.totalCost)} />
              <Stat label="Input tokens" value={formatTokens(analysis.inputTokens)} />
              <Stat label="Output tokens" value={formatTokens(analysis.outputTokens)} />
              <Stat label="Cached tokens" value={formatTokens(analysis.cachedTokens)} />
              <Stat label="Cache hit" value={analysis.cachePercent === undefined ? '—' : analysis.cachePercent.toFixed(1) + '%'} />
              <Stat label="Total tokens" value={formatTokens(analysis.totalTokens)} />
              <Stat label="Messages" value={analysis.messageCount.toLocaleString()} />
              <Stat label="Assistant messages" value={analysis.assistantMessageCount.toLocaleString()} />
              <Stat label="Tool calls" value={analysis.toolCallCount.toLocaleString()} />
            </dl>
          </section>
          {currentMessage ? <CurrentMessageStats record={currentMessage} /> : null}
          </div>
          {replaying ? <nav className="session-analyzer-replay-controls" aria-label="Replay controls">
            <p role="status" aria-live="polite">Message {(currentIndex ?? -1) + 1} of {ordered.length}</p>
            <div>
              <button type="button" aria-label="Previous message" disabled={currentIndex === null || currentIndex <= 0} onClick={() => setReplayIndex(index => Math.max(0, (index ?? 0) - 1))}><StepBackIcon size={24} aria-hidden="true" /><span>Previous</span></button>
              <button type="button" aria-label="Next message" disabled={currentIndex === null || currentIndex >= ordered.length - 1} onClick={() => setReplayIndex(index => Math.min(ordered.length - 1, (index ?? 0) + 1))}><StepForwardIcon size={24} aria-hidden="true" /><span>Next</span></button>
            </div>
          </nav> : null}
        </aside>
      </main>
    </div>
  )
}

const AnalyzerTranscript = memo(function AnalyzerTranscript({ messages, currentIndex }: { messages: readonly SessionMessage[]; currentIndex: number | null }) {
  return <div className="session-analyzer-transcript-content">{messages.map((message, index) => {
    const concealed = currentIndex !== null && index > currentIndex
    return <div key={message.message_id} data-message-index={index} className={`session-analyzer-replay-message${concealed ? ' is-concealed' : ''}`} aria-current={index === currentIndex ? 'step' : undefined}>
      <div inert={concealed} aria-hidden={concealed || undefined}><AnalyzerMessage record={message} /></div>
    </div>
  })}</div>
})

const AnalyzerMessage = memo(function AnalyzerMessage({ record }: { record: SessionMessage }) {
  const body = record.message
  if (body.role === 'user') {
    return <Message className="session-analyzer-message" from="user"><MessageContent className="project-session-user-message session-analyzer-user-message"><RichContent content={body.content} /></MessageContent><MessageMeta label="User" record={record} /></Message>
  }
  if (body.role === 'assistant') return <AssistantTranscriptMessage body={body} record={record} />
  if (body.role === 'tool_result') return <ToolResultItem body={body} record={record} />
  if (body.role === 'system') {
    return <section className="session-analyzer-event"><MessageMeta label="System" record={record} /> <RichContent content={body.content} /></section>
  }
  return <section className="session-analyzer-event"><MessageMeta label={body.tag ? `Custom · ${body.tag}` : 'Custom'} record={record} /><pre>{formatStructuredValue(body.content)}</pre></section>
})

function CurrentMessageStats({ record }: { record: SessionMessage }) {
  const body = record.message
  const stats = buildSessionAnalysis([record], [])
  const role = body.role === 'tool_result' ? 'Tool result' : body.role.charAt(0).toUpperCase() + body.role.slice(1)
  return <section className="session-analyzer-stats" aria-labelledby="session-analyzer-message-stats-title">
    <h2 id="session-analyzer-message-stats-title">This message</h2>
    <dl>
      <Stat label="Type" value={role} />
      <Stat label="Revision" value={`#${record.revision}`} />
      {body.role === 'assistant' ? <>
        <Stat label="Model" value={body.model.name ?? body.model.id} />
        <Stat label="Latency" value={formatLatency(body.duration_ms)} />
        <Stat label="Cost" value={formatCost(stats.totalCost)} />
        <Stat label="Input tokens" value={formatTokens(stats.inputTokens)} />
        <Stat label="Output tokens" value={formatTokens(stats.outputTokens)} />
        <Stat label="Cached tokens" value={formatTokens(stats.cachedTokens)} />
        <Stat label="Cache hit" value={stats.cachePercent === undefined ? '—' : stats.cachePercent.toFixed(1) + '%'} />
        <Stat label="Total tokens" value={formatTokens(stats.totalTokens)} />
        <Stat label="Tool calls" value={String(stats.toolCallCount)} />
      </> : body.role === 'tool_result' ? <>
        <Stat label="Tool" value={body.tool_name} />
        <Stat label="Status" value={body.outcome.status} />
      </> : null}
    </dl>
    {body.role !== 'assistant' ? <p className="session-analyzer-stats-note">Token usage, cost and latency are recorded on assistant messages.</p> : null}
  </section>
}

function AssistantTranscriptMessage({ body, record }: { body: AssistantMessage; record: SessionMessage }) {
  return <Message className="session-analyzer-message" from="assistant">
    <MessageContent className="project-session-assistant-message session-analyzer-assistant-message">
      {body.content.map((part, index) => <Fragment key={`${part.type}:${index}`}>
        {part.type === 'response' ? <MessageResponse className="project-session-markdown">{part.response.content}</MessageResponse>
          : part.type === 'thinking' ? <section className="session-analyzer-thinking"><span>Thinking</span>{part.thinking_text.trim() ? <MessageResponse className="project-session-markdown">{part.thinking_text}</MessageResponse> : null}</section>
          : <ToolCallItem name={part.name} argumentsValue={part.arguments} toolCallId={part.tool_call_id} />}
      </Fragment>)}
    </MessageContent>
    <MessageMeta label={body.model.name ?? body.model.id ?? 'Assistant'} record={record} detail={formatAssistantDetail(body)} />
  </Message>
}

function ToolCallItem({ name, argumentsValue, toolCallId }: { name: string; argumentsValue: unknown; toolCallId: string }) {
  return <section className="session-analyzer-tool" data-kind="call">
    <header><WrenchIcon aria-hidden="true" size={13} /><span>Tool call</span><strong>{name}</strong></header>
    <pre>{formatStructuredValue(argumentsValue)}</pre>
    <span className="session-analyzer-tool-id">{toolCallId}</span>
  </section>
}

function ToolResultItem({ body, record }: { body: ToolResultMessage; record: SessionMessage }) {
  return <section className="session-analyzer-tool session-analyzer-tool-result" data-status={body.outcome.status}>
    <header><span>Tool result</span><strong>{body.tool_name}</strong><time dateTime={record.created_at}>{formatTime(record.created_at)}</time></header>
    {body.outcome.status === 'error' ? <p className="session-analyzer-tool-error">{body.outcome.error.message}</p> : null}
    <RichContent content={body.content} plain />
    {body.content.length === 0 && body.details !== undefined ? <pre>{formatStructuredValue(body.details)}</pre> : null}
  </section>
}

function RichContent({ content, plain = false }: { content: readonly ({ type: 'text'; content: string } | ({ type: 'image' } & ImageContent))[]; plain?: boolean }) {
  return <>{content.map((part, index) => part.type === 'text'
    ? plain ? <pre key={`text:${index}`}>{part.content}</pre> : <MessageResponse className="project-session-markdown" key={`text:${index}`}>{part.content}</MessageResponse>
    : <img className="session-analyzer-image" key={`image:${index}`} src={part.source.type === 'url' ? part.source.url : `data:${part.source.mime_type};base64,${part.source.data}`} alt="Session attachment" loading="lazy" />)}</>
}

function MessageMeta({ label, record, detail }: { label: string; record: SessionMessage; detail?: string }) {
  return <div className="session-analyzer-message-meta"><strong>{label}</strong>{detail ? <span>{detail}</span> : null}<time dateTime={record.created_at}>{formatTime(record.created_at)}</time><span>#{record.revision}</span></div>
}

function AnalysisChart({ title, points, formatValue }: { title: string; points: readonly SessionAnalysisPoint[]; formatValue: (value: number) => string }) {
  const width = 320
  const height = 132
  const inset = { top: 12, right: 10, bottom: 22, left: 38 }
  const plotWidth = width - inset.left - inset.right
  const plotHeight = height - inset.top - inset.bottom
  const maximum = Math.max(0, ...points.map(point => point.value))
  const minimumMessage = points[0]?.messageNumber ?? 0
  const maximumMessage = points.at(-1)?.messageNumber ?? minimumMessage
  const coordinates = points.map((point, index) => {
    const x = inset.left + (points.length < 2 ? plotWidth / 2 : index / (points.length - 1) * plotWidth)
    const y = inset.top + plotHeight - (maximum === 0 ? 0 : point.value / maximum * plotHeight)
    return { ...point, x, y }
  })
  const path = coordinates.map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x.toFixed(2)} ${point.y.toFixed(2)}`).join(' ')

  return <section className="session-analyzer-chart">
    <header><h2>{title}</h2>{points.length ? <strong>{formatValue(points.at(-1)?.value ?? 0)}</strong> : null}</header>
    {points.length === 0 ? <div className="session-analyzer-chart-empty">No recorded data</div> : <svg viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`${title}. Latest value ${formatValue(points.at(-1)?.value ?? 0)}.`}>
      {[0, 0.5, 1].map(ratio => <line key={ratio} className="session-analyzer-chart-grid" x1={inset.left} x2={width - inset.right} y1={inset.top + plotHeight * ratio} y2={inset.top + plotHeight * ratio} />)}
      <text x={inset.left - 6} y={inset.top + 4} textAnchor="end">{formatValue(maximum)}</text>
      <text x={inset.left - 6} y={inset.top + plotHeight + 4} textAnchor="end">{formatValue(0)}</text>
      <text x={inset.left} y={height - 4}>#{minimumMessage}</text>
      <text x={width - inset.right} y={height - 4} textAnchor="end">#{maximumMessage}</text>
      <path className="session-analyzer-chart-line" d={path} />
      {coordinates.map(point => <circle className="session-analyzer-chart-point" key={point.messageNumber} cx={point.x} cy={point.y} r="2.5"><title>{`Message ${point.messageNumber}: ${formatValue(point.value)}`}</title></circle>)}
    </svg>}
  </section>
}

function Stat({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>
}

function AnalyzerState({ message, loading = false, onRetry }: { message: string; loading?: boolean; onRetry?: () => void }) {
  return <div className="session-analyzer-state" role="status">{loading ? <LoaderCircleIcon className="project-session-spinner" size={16} aria-hidden="true" /> : null}<span>{message}</span>{onRetry ? <button type="button" onClick={onRetry}>Retry</button> : null}</div>
}

function formatAssistantDetail(message: AssistantMessage) {
  const parts = [formatLatency(message.duration_ms)]
  if (message.usage?.cost) parts.push(formatCost(message.usage.cost.total))
  const usage = message.usage
  if (usage && [usage.input, usage.output, usage.cache_read, usage.cache_write].some(value => value !== undefined)) {
    parts.push(formatTokens((usage.input ?? 0) + (usage.output ?? 0) + (usage.cache_read ?? 0) + (usage.cache_write ?? 0)) + ' tokens')
  }
  return parts.join(' · ')
}

function formatStructuredValue(value: unknown) {
  if (typeof value === 'string') return value
  try { return JSON.stringify(value, null, 2) } catch { return String(value) }
}

function formatTime(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date)
}

function formatDuration(milliseconds: number) {
  if (milliseconds < 1_000) return `${Math.round(milliseconds)}ms`
  const seconds = milliseconds / 1_000
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`
  const minutes = Math.floor(seconds / 60)
  const remainingSeconds = Math.floor(seconds % 60)
  if (minutes < 60) return `${minutes}m ${remainingSeconds}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

function formatLatency(value: number) {
  return value < 1_000 ? `${Math.round(value)}ms` : `${(value / 1_000).toFixed(1)}s`
}

function formatCost(value: number | undefined) {
  return value === undefined ? '—' : '$' + value.toFixed(5)
}

function formatChartCost(value: number) {
  return value === 0 ? '$0' : '$' + value.toFixed(value < 0.01 ? 4 : 2)
}

function formatTokens(value: number | undefined) {
  return value === undefined ? '—' : Math.round(value).toLocaleString()
}
