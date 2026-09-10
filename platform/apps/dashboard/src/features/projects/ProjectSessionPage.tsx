import { memo, useCallback, useMemo, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useParams } from 'react-router-dom'
import { LoaderCircleIcon } from 'lucide-react'
import { CloudIcon, ComputerIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { ProviderIcon } from '../providers/provider-icons'
import { postJson } from '../../lib/api-client'
import { ProjectEnvironmentConversation } from './ProjectEnvironmentConversation'
import { ProjectEnvironmentPromptComposer } from './ProjectEnvironmentPromptComposer'
import { ProjectRunDetailsDrawer } from './ProjectRunDetailsDrawer'
import { chatInputsOptions, chatMessagesOptions, chatRunsOptions, refreshChat, retryRead, seedAccepted, useChatSession, validId } from './chat-queries'
import { lockedChatOptions, sessionReasoningLevels } from './chat-config'
import { useChatStream } from './chat-stream'
import { useChatSubmission, userMessage } from './chat-submission'
import { buildConversation } from './project-conversation'
import { useProjectBootstrap } from './project-queries'
import { summarizeSessionUsage } from './project-session-usage'
import type { ChatSession, ProjectRunSummary, RunInput, SessionMessage } from './project-session-types'
import { isLiveRunStatus } from './project-session-types'

const EMPTY_MESSAGES: SessionMessage[] = []
const EMPTY_RUNS: ProjectRunSummary[] = []
const EMPTY_INPUTS: RunInput[] = []

export default function ProjectSessionPage() {
  const { projectId = '', sessionId = '' } = useParams()
  if (!validId(sessionId)) return <p role="alert">Invalid chat ID.</p>
  return <SessionView key={sessionId} projectId={projectId.toLowerCase()} sessionId={sessionId.toLowerCase()} />
}

function SessionView({ projectId, sessionId }: { projectId: string; sessionId: string }) {
  const client = useQueryClient()
  const session = useChatSession(sessionId)
  const live = Boolean(session.data?.active_run)
  const enabled = session.data?.project_id === projectId
  const messages = useQuery({ ...chatMessagesOptions(client, sessionId, live), enabled })
  const runs = useQuery({ ...chatRunsOptions(sessionId, live), enabled })
  const inputs = useQuery({ ...chatInputsOptions(sessionId, live), enabled })
  const reconnecting = useChatStream(projectId, sessionId, enabled ? session.data?.active_run?.id : undefined)
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null)
  const [isClosing, setIsClosing] = useState(false)
  const detailsTrigger = useRef<HTMLElement | null>(null)
  const [submittedSequence, setSubmittedSequence] = useState(0)
  const didSubmit = useCallback(() => setSubmittedSequence(value => value + 1), [])
  const openDetails = useCallback((id: string) => { detailsTrigger.current = document.activeElement as HTMLElement; setSelectedRunId(id); setIsClosing(false) }, [])
  const closeDetails = useCallback(() => setIsClosing(true), [])
  const finishClose = useCallback(() => { setSelectedRunId(null); setIsClosing(false); detailsTrigger.current?.focus({ preventScroll: true }) }, [])
  const retry = useCallback(() => { void refreshChat(client, projectId, sessionId) }, [client, projectId, sessionId])
  const runItems = useMemo(() => {
    const list = runs.data ?? EMPTY_RUNS
    const active = session.data?.active_run
    if (!active) return list
    const existing = list.find(r => r.id === active.id)
    // Whichever read is newer wins during lifecycle races.
    if (existing && existing.version >= active.version) return list
    return [...list.filter(r => r.id !== active.id), active].sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id))
  }, [runs.data, session.data?.active_run])
  const history = messages.data ?? EMPTY_MESSAGES
  const items = useMemo(() => buildConversation(history, runItems, inputs.data ?? EMPTY_INPUTS), [history, runItems, inputs.data])
  const selectedRun = runItems.find(r => r.id === selectedRunId)
  const activeRun = runItems.find(r => isLiveRunStatus(r.status)) ?? null
  if (!session.data) return <div className="project-session-conversation-state" role="status">{session.isError ? <><span>{session.error.message}</span><button onClick={() => void session.refetch()}>Retry</button></> : session.fetchStatus === 'paused' ? 'You’re offline. Reconnect to load this chat.' : <LoaderCircleIcon className="project-session-spinner" size={16} aria-label="Loading chat" />}</div>
  if (!enabled) return <p role="alert">This chat does not belong to this project.</p>
  const readError = session.isError || messages.isError || runs.isError || inputs.isError
  return (
    <div className="project-session-page">
      <div className="project-session-workspace">
        <SessionHeader session={session.data} activeRun={activeRun} messages={history} />
        <div className="project-session-body">
          <div className="project-session-main-pane">
            {readError || reconnecting ? <div className="project-chat-sync-notice" role="status">{readError ? <>Couldn’t refresh chat. <button onClick={retry}>Retry</button></> : 'Reconnecting live updates…'}</div> : null}
            <ProjectEnvironmentConversation sessionKey={sessionId} submittedSequence={submittedSequence} items={items} isPending={items.length === 0 && (messages.isPending || runs.isPending || inputs.isPending)} isError={readError} onRetry={retry} onOpenRunDetails={openDetails} />
            <SessionComposer session={session.data} activeRun={activeRun} ready={messages.isSuccess && runs.isSuccess && inputs.isSuccess} onAccepted={didSubmit} />
          </div>
        </div>
        {selectedRun ? <div className={'project-run-drawer-slot' + (isClosing ? ' project-run-drawer-slot--closing' : '')}><ProjectRunDetailsDrawer key={selectedRun.id} run={selectedRun} messages={history} isClosing={isClosing} onClose={closeDetails} onClosed={finishClose} /></div> : null}
      </div>
    </div>
  )
}

const SessionComposer = memo(function SessionComposer({ session, activeRun, ready, onAccepted }: { session: ChatSession; activeRun: ProjectRunSummary | null; ready: boolean; onAccepted: () => void }) {
  const client = useQueryClient()
  const bootstrap = useProjectBootstrap(session.project_id)
  const [prompt, setPrompt] = useState('')
  const locked = useMemo(() => lockedChatOptions(session.config), [session.config])
  const submission = useChatSubmission('session:' + session.id, reply => {
    seedAccepted(client, session.project_id, session.id, reply)
    setPrompt('')
    onAccepted()
  }, () => { void refreshChat(client, session.project_id, session.id) })
  const stop = useMutation({
    mutationFn: ({ runId, key }: { runId: string; key: string }) => postJson('/api/runs/' + runId + '/abort', { reason: 'user_requested' }, key),
    retry: retryRead, retryDelay: n => 1000 * 2 ** n,
    onSettled: () => { void refreshChat(client, session.project_id, session.id) },
  })
  const accountName = bootstrap.data?.provider_accounts.find(a => a.id === locked?.accountId)?.name ?? 'Session account'
  const accounts = useMemo(() => locked ? [{ account_id: locked.accountId, name: accountName, provider: locked.provider, model_ids: [locked.modelId] }] : [], [locked, accountName])
  const harness = bootstrap.data?.harnesses.find(h => h.id === session.harness_id)
  const levels = useMemo(() => sessionReasoningLevels(harness, locked?.reasoningLevel), [harness, locked?.reasoningLevel])
  const refetchOptions = bootstrap.refetch
  const retryOptions = useCallback(() => { void refetchOptions() }, [refetchOptions])
  return <div className="project-session-composer-wrap">
    {submission.uncertain ? <p className="project-chat-send-notice" role="alert">The last send is not confirmed. <button onClick={submission.retry}>Retry the same send</button></p> : null}
    <ProjectEnvironmentPromptComposer
      promptValue={prompt} onPromptChange={setPrompt}
      promptLabel={session.harness_id === 'sites' ? 'Site instructions' : undefined}
      placeholder={activeRun ? 'Add a message to this run…' : 'Send a message…'}
      providerAccounts={accounts} reasoningLevels={levels} lockedOptions={locked ?? undefined} optionsReadOnly
      webSearchSupported={typeof session.config.web_search_enabled === 'boolean'}
      isModelOptionsPending={false} isModelOptionsError={false} onRetryModelOptions={retryOptions}
      isSubmissionReady={ready && !!locked && !session.archived_at && !submission.uncertain}
      isSubmitting={submission.isPending} isRunActive={!!activeRun} allowSteering
      isStopping={stop.isPending || !!activeRun?.abort_requested_at}
      submitError={submission.error?.message ?? stop.error?.message ?? (!locked ? 'This session configuration cannot be used by this composer.' : null)}
      onSubmit={value => {
        const input = userMessage(value.prompt)
        if (activeRun) submission.send('/api/runs/' + activeRun.id + '/inputs', { kind: 'user_message', message: input })
        else submission.send('/api/sessions/' + session.id + '/runs', { input, expected_session_revision: session.current_revision })
      }}
      onStop={() => { if (activeRun && !stop.isPending) stop.mutate({ runId: activeRun.id, key: crypto.randomUUID() }) }}
    />
  </div>
})

const SessionHeader = memo(function SessionHeader({ session, activeRun, messages }: { session: ChatSession; activeRun: ProjectRunSummary | null; messages: readonly SessionMessage[] }) {
  const bootstrap = useProjectBootstrap(session.project_id)
  const locked = lockedChatOptions(session.config)
  const account = bootstrap.data?.provider_accounts.find(a => a.id === locked?.accountId)
  const descriptor = session.config.environment as { type?: string; machine_id?: string; snapshot_id?: string; workspace_root?: string; path?: string } | undefined
  const environment = bootstrap.data?.project_environments.find(e => e.type === descriptor?.type && e.workspace_root === descriptor.workspace_root && e.path === descriptor.path && (e.type === 'machine' ? e.machine_id === descriptor.machine_id : e.snapshot_id === descriptor.snapshot_id))
  const usage = useMemo(() => summarizeSessionUsage(messages), [messages])
  return <header className="project-session-header">
    <div className="project-session-context">
      <span className="project-context-picker-trigger project-context-picker-trigger--static" title={session.harness_id}>{session.harness_id}</span>
      {session.harness_id === 'sites' ? (session.site_id || typeof session.config.siteId === 'string' ? <Link className="project-context-picker-trigger" to={`/projects/${session.project_id}/sites/${session.site_id || session.config.siteId}`}>View site</Link> : <span className="provider-detail">Site created when authoring begins</span>) : null}
      {descriptor ? <span className="project-context-picker-trigger project-context-picker-trigger--static" title={environment?.name ?? descriptor.path}>
        <HugeiconsIcon icon={descriptor.type === 'machine' ? ComputerIcon : CloudIcon} size={13} strokeWidth={1.5} aria-hidden="true" />
        <span>{environment?.name ?? descriptor.path ?? 'Environment'}</span>
      </span> : null}
    </div>
    <div className="project-session-header-summary">
      <Link className="project-session-visualizer-link" to={`/projects/${session.project_id}/${session.id}/analyze`}>Analyze</Link>
      {locked ? <span className="project-session-header-account" title={'Provider account: ' + (account?.name ?? 'Session account')}><ProviderIcon provider={locked.provider} className="project-session-header-account-icon" /><span>{account?.name ?? 'Session account'}</span></span> : null}
      <span className="project-session-header-metric" title={'Recorded cost: ' + usage.cost + ' · Cache hit rate: ' + usage.cache + ' · Latest context: ' + usage.context + ' tokens'}>{usage.cost}<span className="project-session-header-metric-separator">/</span>{usage.cache}<span className="project-session-header-metric-separator">/</span>{usage.context}</span>
      <span role="status">{activeRun?.abort_requested_at ? 'Stopping…' : activeRun?.status === 'ready' ? 'Queued' : activeRun?.status === 'waiting' ? 'Waiting' : activeRun ? 'Working' : 'Idle'}</span>
    </div>
  </header>
})
