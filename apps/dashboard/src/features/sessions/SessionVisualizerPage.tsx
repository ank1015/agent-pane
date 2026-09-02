import { MessageResponse } from '@/components/ai-elements/message'
import {
  ArrowLeftIcon,
  BotIcon,
  BracesIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  Clock3Icon,
  CoinsIcon,
  DatabaseIcon,
  LoaderCircleIcon,
  RefreshCwIcon,
  ShieldIcon,
  UserRoundIcon,
  WrenchIcon,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { Link } from 'react-router-dom'
import type {
  AssistantContent,
  AssistantUsage,
  ImageContent,
  MessageContentPart,
  SessionMessage,
} from '../projects/project-session-types'
import {
  buildSessionVisualizerSteps,
  textFromMessage,
  usageFromMessage,
} from './session-visualizer-model'
import type { SessionVisualizerStep } from './session-visualizer-model'
import {
  useSession,
  useSessionMessages,
  useSessionRuns,
} from './session-queries'
import './session-visualizer.css'

type SessionVisualizerPageProps = {
  sessionId: string
}

const ROLE_META = {
  user: { label: 'User', icon: UserRoundIcon, tone: 'blue' },
  assistant: { label: 'Assistant', icon: BotIcon, tone: 'violet' },
  tool_result: { label: 'Tool result', icon: WrenchIcon, tone: 'amber' },
  system: { label: 'System', icon: ShieldIcon, tone: 'rose' },
  custom: { label: 'Custom', icon: BracesIcon, tone: 'slate' },
} as const

export function SessionVisualizerPage({
  sessionId,
}: SessionVisualizerPageProps) {
  const session = useSession(sessionId)
  const messages = useSessionMessages(sessionId)
  const runs = useSessionRuns(sessionId)
  const [selectedRevision, setSelectedRevision] = useState<number | null>(null)
  const messageItems = useMemo(
    () => messages.data?.pages.flatMap((page) => page.items) ?? [],
    [messages.data],
  )
  const runItems = useMemo(
    () => runs.data?.pages.flatMap((page) => page.items) ?? [],
    [runs.data],
  )
  const steps = useMemo(
    () => buildSessionVisualizerSteps(messageItems),
    [messageItems],
  )
  const latestRevision = steps.at(-1)?.message.revision ?? 0
  const activeRevision = selectedRevision === null
    ? latestRevision
    : Math.min(selectedRevision, latestRevision)
  const visibleSteps = useMemo(
    () => steps.filter((step) => step.message.revision <= activeRevision),
    [activeRevision, steps],
  )
  const activeStep = visibleSteps.at(-1)
  const totals = summarizeSteps(visibleSteps)
  const completeTotals = summarizeSteps(steps)
  const fetchNextMessagePage = messages.fetchNextPage
  const fetchNextRunPage = runs.fetchNextPage

  useEffect(() => {
    if (messages.hasNextPage && !messages.isFetchingNextPage) {
      void fetchNextMessagePage()
    }
  }, [fetchNextMessagePage, messages.hasNextPage, messages.isFetchingNextPage])

  useEffect(() => {
    if (runs.hasNextPage && !runs.isFetchingNextPage) {
      void fetchNextRunPage()
    }
  }, [fetchNextRunPage, runs.hasNextPage, runs.isFetchingNextPage])

  const refresh = () => {
    void Promise.all([session.refetch(), messages.refetch(), runs.refetch()])
  }

  const isLoading = session.isPending || messages.isPending || runs.isPending
  const hasError = session.isError || messages.isError || runs.isError

  return (
    <div className="session-visualizer-shell">
      <header className="session-visualizer-topbar">
        <div className="session-visualizer-title-group">
          <Link
            className="session-visualizer-back"
            to={session.data === undefined
              ? '/projects'
              : `/projects/${encodeURIComponent(session.data.session.project_id)}/${encodeURIComponent(sessionId)}`}
          >
            <ArrowLeftIcon aria-hidden="true" size={15} />
            Session
          </Link>
          <span className="session-visualizer-title-divider" aria-hidden="true" />
          <div>
            <div className="session-visualizer-eyebrow">Session visualizer</div>
            <h1>{session.data?.session.title ?? 'Loading session…'}</h1>
          </div>
        </div>
        <div className="session-visualizer-topbar-actions">
          {session.data === undefined ? null : (
            <span
              className="session-visualizer-status"
              data-live={session.data.session.is_active}
            >
              <span aria-hidden="true" />
              {session.data.session.is_active ? 'Live' : 'Complete'}
            </span>
          )}
          <button type="button" onClick={refresh}>
            <RefreshCwIcon aria-hidden="true" size={14} />
            Refresh
          </button>
        </div>
      </header>

      {isLoading ? (
        <PageState icon={<LoaderCircleIcon className="session-visualizer-spinner" size={18} />}>
          Loading the complete session…
        </PageState>
      ) : hasError ? (
        <PageState icon={<CircleAlertIcon size={18} />}>
          <span>Couldn’t load the session visualizer.</span>
          <button type="button" onClick={refresh}>Try again</button>
        </PageState>
      ) : steps.length === 0 ? (
        <PageState icon={<DatabaseIcon size={18} />}>
          No committed messages were found for this session.
        </PageState>
      ) : (
        <div className="session-visualizer-content">
          <section className="session-visualizer-overview" aria-label="Session metrics">
            <MetricCard
              icon={<CoinsIcon size={16} />}
              label="Cost at this step"
              value={formatCost(totals.cost)}
              detail={`${formatCost(completeTotals.cost)} total`}
            />
            <MetricCard
              icon={<Clock3Icon size={16} />}
              label="Observed latency"
              value={formatDuration(totals.latencyMs)}
              detail={`${formatDuration(completeTotals.latencyMs)} total`}
            />
            <MetricCard
              icon={<DatabaseIcon size={16} />}
              label="Tokens processed"
              value={formatCompactNumber(totals.tokens)}
              detail={`${formatCompactNumber(completeTotals.tokens)} total`}
            />
            <MetricCard
              icon={<BracesIcon size={16} />}
              label="Progress"
              value={`${visibleSteps.length} / ${steps.length}`}
              detail={`${runItems.length} ${runItems.length === 1 ? 'run' : 'runs'}`}
            />
          </section>

          <section className="session-visualizer-chart-card">
            <div className="session-visualizer-chart-header">
              <div>
                <span>Progression</span>
                <strong>Cost and latency across revisions</strong>
              </div>
              <div className="session-visualizer-legend" aria-label="Chart legend">
                <span data-series="cost"><i />Cost</span>
                <span data-series="latency"><i />Latency</span>
              </div>
            </div>
            <ProgressChart steps={steps} selectedRevision={activeRevision} />
            <div className="session-visualizer-scrubber">
              <button
                type="button"
                aria-label="Previous revision"
                disabled={activeRevision <= (steps[0]?.message.revision ?? 1)}
                onClick={() => setSelectedRevision(previousRevision(steps, activeRevision))}
              >
                <ChevronLeftIcon aria-hidden="true" size={15} />
              </button>
              <label>
                <span>Revision {activeRevision} of {latestRevision}</span>
                <input
                  type="range"
                  min={steps[0]?.message.revision ?? 1}
                  max={latestRevision}
                  value={activeRevision}
                  onChange={(event) => setSelectedRevision(Number(event.target.value))}
                />
              </label>
              <button
                type="button"
                aria-label="Next revision"
                disabled={activeRevision >= latestRevision}
                onClick={() => {
                  const next = nextRevision(steps, activeRevision)
                  setSelectedRevision(next === latestRevision ? null : next)
                }}
              >
                <ChevronRightIcon aria-hidden="true" size={15} />
              </button>
              <button
                className="session-visualizer-latest"
                type="button"
                disabled={activeRevision === latestRevision}
                onClick={() => setSelectedRevision(null)}
              >
                Jump to latest
              </button>
            </div>
          </section>

          <div className="session-visualizer-workspace">
            <section className="session-visualizer-timeline" aria-label="Session transcript">
              <header>
                <div>
                  <span>Transcript</span>
                  <strong>{visibleSteps.length} visible steps</strong>
                </div>
                <span>Ordered by canonical revision</span>
              </header>
              <div className="session-visualizer-timeline-list">
                {visibleSteps.map((step) => (
                  <TimelineStep
                    key={step.message.session_message_id}
                    step={step}
                    selected={step.message.revision === activeRevision}
                    onSelect={() => setSelectedRevision(step.message.revision)}
                  />
                ))}
              </div>
            </section>

            <aside className="session-visualizer-inspector" aria-label="Selected step details">
              {activeStep === undefined ? null : (
                <StepInspector step={activeStep} />
              )}
            </aside>
          </div>
        </div>
      )}
    </div>
  )
}

function MetricCard({
  detail,
  icon,
  label,
  value,
}: {
  detail: string
  icon: ReactNode
  label: string
  value: string
}) {
  return (
    <article className="session-visualizer-metric">
      <div className="session-visualizer-metric-icon" aria-hidden="true">{icon}</div>
      <div>
        <span>{label}</span>
        <strong>{value}</strong>
        <small>{detail}</small>
      </div>
    </article>
  )
}

function ProgressChart({
  selectedRevision,
  steps,
}: {
  selectedRevision: number
  steps: readonly SessionVisualizerStep[]
}) {
  const width = 1000
  const height = 176
  const padding = 12
  const costValues = steps.map((step) => step.cumulativeCost)
  const latencyValues = steps.map((step) => step.cumulativeLatencyMs)
  const costPath = chartPath(costValues, width, height, padding)
  const latencyPath = chartPath(latencyValues, width, height, padding)
  const selectedIndex = Math.max(
    0,
    steps.findIndex((step) => step.message.revision === selectedRevision),
  )
  const selectedX = chartX(selectedIndex, steps.length, width, padding)

  return (
    <div className="session-visualizer-chart">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        role="img"
        aria-label="Cumulative cost and latency by session revision"
      >
        <defs>
          <linearGradient id="session-cost-fill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="#8b7bff" stopOpacity="0.24" />
            <stop offset="1" stopColor="#8b7bff" stopOpacity="0" />
          </linearGradient>
        </defs>
        {[0.25, 0.5, 0.75].map((position) => (
          <line
            key={position}
            className="session-visualizer-chart-grid"
            x1="0"
            x2={width}
            y1={height * position}
            y2={height * position}
          />
        ))}
        <path
          className="session-visualizer-chart-area"
          d={`${costPath} L ${width - padding} ${height - padding} L ${padding} ${height - padding} Z`}
        />
        <path className="session-visualizer-chart-line session-visualizer-chart-line--cost" d={costPath} />
        <path className="session-visualizer-chart-line session-visualizer-chart-line--latency" d={latencyPath} />
        <line
          className="session-visualizer-chart-cursor"
          x1={selectedX}
          x2={selectedX}
          y1="0"
          y2={height}
        />
      </svg>
    </div>
  )
}

function TimelineStep({
  onSelect,
  selected,
  step,
}: {
  onSelect: () => void
  selected: boolean
  step: SessionVisualizerStep
}) {
  const body = step.message.message
  const meta = ROLE_META[body.role]
  const Icon = meta.icon
  const preview = textFromMessage(step.message).trim()

  return (
    <button
      className="session-visualizer-step"
      data-role={meta.tone}
      data-selected={selected}
      type="button"
      onClick={onSelect}
    >
      <span className="session-visualizer-step-rail" aria-hidden="true">
        <i><Icon size={14} /></i>
      </span>
      <span className="session-visualizer-step-body">
        <span className="session-visualizer-step-heading">
          <span>
            <strong>{step.label}</strong>
            <small>Revision {step.message.revision}</small>
          </span>
          <span className="session-visualizer-step-metrics">
            {step.cost > 0 ? <small>{formatCost(step.cost)}</small> : null}
            {step.latencyMs > 0 ? (
              <small>{step.latencyEstimated ? '~' : ''}{formatDuration(step.latencyMs)}</small>
            ) : null}
          </span>
        </span>
        <span className="session-visualizer-step-preview">
          {preview.length === 0 ? 'No visible content' : preview}
        </span>
        <span className="session-visualizer-step-footer">
          <small>{formatTimestamp(body.timestamp)}</small>
          {step.message.run_id === null ? null : (
            <small>Run {shortId(step.message.run_id)}</small>
          )}
          {step.message.turn_number === null ? null : (
            <small>Turn {step.message.turn_number}</small>
          )}
        </span>
      </span>
    </button>
  )
}

function StepInspector({ step }: { step: SessionVisualizerStep }) {
  const message = step.message
  const body = message.message
  const meta = ROLE_META[body.role]
  const usage = usageFromMessage(message)

  return (
    <div className="session-visualizer-inspector-inner">
      <header>
        <span>Selected step</span>
        <strong>{step.label}</strong>
        <small>{meta.label} · revision {message.revision}</small>
      </header>
      <dl className="session-visualizer-inspector-stats">
        <InspectorStat label="Step cost" value={step.cost > 0 ? formatCost(step.cost) : '—'} />
        <InspectorStat
          label={step.latencyEstimated ? 'Estimated latency' : 'Latency'}
          value={step.latencyMs > 0 ? formatDuration(step.latencyMs) : '—'}
        />
        <InspectorStat label="Cumulative cost" value={formatCost(step.cumulativeCost)} />
        <InspectorStat label="Cumulative latency" value={formatDuration(step.cumulativeLatencyMs)} />
      </dl>
      {usage === undefined ? null : <UsageBreakdown usage={usage} />}
      <section className="session-visualizer-inspector-content">
        <h2>Message</h2>
        <MessageContent message={message} />
      </section>
      <section className="session-visualizer-inspector-meta">
        <h2>Metadata</h2>
        <dl>
          <div><dt>Origin</dt><dd>{message.origin}</dd></div>
          <div><dt>Delivery</dt><dd>{message.delivery}</dd></div>
          <div><dt>Message ID</dt><dd title={message.session_message_id}>{shortId(message.session_message_id)}</dd></div>
          <div><dt>Committed</dt><dd>{formatIsoTimestamp(message.committed_at)}</dd></div>
        </dl>
      </section>
    </div>
  )
}

function InspectorStat({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>
}

function UsageBreakdown({ usage }: { usage: AssistantUsage }) {
  const entries = [
    ['Input', usage.input],
    ['Output', usage.output],
    ['Cache read', usage.cache_read],
    ['Cache write', usage.cache_write],
  ] as const
  return (
    <section className="session-visualizer-usage">
      <h2>Token usage</h2>
      <div>
        {entries.map(([label, value]) => (
          <span key={label}><small>{label}</small><strong>{formatCompactNumber(value ?? 0)}</strong></span>
        ))}
      </div>
    </section>
  )
}

function MessageContent({ message }: { message: SessionMessage }) {
  const body = message.message
  if (body.role === 'assistant') {
    return <AssistantParts parts={body.content} />
  }
  if (body.role === 'custom') {
    return <pre>{JSON.stringify(body.content, null, 2)}</pre>
  }
  if (body.role === 'tool_result') {
    return (
      <div className="session-visualizer-tool-content">
        <span data-status={body.outcome.status}>{body.outcome.status}</span>
        {body.outcome.status === 'error' ? (
          <p className="session-visualizer-tool-error">{body.outcome.error.message}</p>
        ) : null}
        <ContentParts content={body.content} />
        {body.details === undefined ? null : <pre>{JSON.stringify(body.details, null, 2)}</pre>}
      </div>
    )
  }
  if (body.role === 'user') {
    return <ContentParts content={body.content} />
  }
  return <MessageResponse>{textFromMessage(message)}</MessageResponse>
}

function ContentParts({ content }: { content: readonly MessageContentPart[] }) {
  return (
    <div className="session-visualizer-content-parts">
      {content.map((part, index) => part.type === 'text' ? (
        <MessageResponse key={`text:${index}`}>{part.content}</MessageResponse>
      ) : (
        <img
          alt="Message attachment"
          key={`image:${index}`}
          loading="lazy"
          src={imageSource(part)}
        />
      ))}
    </div>
  )
}

function AssistantParts({ parts }: { parts: readonly AssistantContent[] }) {
  return (
    <div className="session-visualizer-assistant-parts">
      {parts.map((part, index) => {
        if (part.type === 'response') {
          return <MessageResponse key={index}>{part.response.content}</MessageResponse>
        }
        if (part.type === 'thinking') {
          return (
            <details key={index}>
              <summary>Thinking</summary>
              <MessageResponse>{part.thinking_text}</MessageResponse>
            </details>
          )
        }
        return (
          <div className="session-visualizer-tool-call" key={index}>
            <strong>{part.name}</strong>
            <pre>{typeof part.arguments === 'string' ? part.arguments : JSON.stringify(part.arguments, null, 2)}</pre>
          </div>
        )
      })}
    </div>
  )
}

function PageState({ children, icon }: { children: ReactNode; icon: ReactNode }) {
  return <div className="session-visualizer-page-state" role="status">{icon}{children}</div>
}

function summarizeSteps(steps: readonly SessionVisualizerStep[]) {
  let tokens = 0
  for (const step of steps) {
    tokens += step.inputTokens + step.outputTokens + step.cacheReadTokens + step.cacheWriteTokens
  }
  const latest = steps.at(-1)
  return {
    cost: latest?.cumulativeCost ?? 0,
    latencyMs: latest?.cumulativeLatencyMs ?? 0,
    tokens,
  }
}

function chartPath(values: readonly number[], width: number, height: number, padding: number) {
  const max = Math.max(...values, 1)
  return values.map((value, index) => {
    const x = chartX(index, values.length, width, padding)
    const y = height - padding - (value / max) * (height - padding * 2)
    return `${index === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`
  }).join(' ')
}

function chartX(index: number, count: number, width: number, padding: number) {
  if (count <= 1) return padding
  return padding + (index / (count - 1)) * (width - padding * 2)
}

function previousRevision(steps: readonly SessionVisualizerStep[], revision: number) {
  return [...steps].reverse().find((step) => step.message.revision < revision)?.message.revision ?? revision
}

function nextRevision(steps: readonly SessionVisualizerStep[], revision: number) {
  return steps.find((step) => step.message.revision > revision)?.message.revision ?? revision
}

function formatCost(value: number) {
  return `$${value.toLocaleString(undefined, { minimumFractionDigits: 4, maximumFractionDigits: 6 })}`
}

function formatDuration(value: number) {
  if (value < 1_000) return `${Math.round(value)}ms`
  if (value < 60_000) return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)}s`
  const roundedSeconds = Math.round(value / 1_000)
  const minutes = Math.floor(roundedSeconds / 60)
  const seconds = roundedSeconds % 60
  return `${minutes}m ${seconds}s`
}

function formatCompactNumber(value: number) {
  return Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(value)
}

function formatTimestamp(value: number) {
  return new Date(value).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

function formatIsoTimestamp(value: string) {
  return new Date(value).toLocaleString([], { dateStyle: 'medium', timeStyle: 'medium' })
}

function shortId(value: string) {
  return `${value.slice(0, 8)}…${value.slice(-4)}`
}

function imageSource(image: ImageContent) {
  return image.source.type === 'url'
    ? image.source.url
    : `data:${image.source.mime_type};base64,${image.source.data}`
}
