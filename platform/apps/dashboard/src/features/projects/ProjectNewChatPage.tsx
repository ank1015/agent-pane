import { useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import { CloudIcon, ComputerIcon } from '@hugeicons/core-free-icons'
import { ProjectEnvironmentPromptComposer } from './ProjectEnvironmentPromptComposer'
import { ProjectContextPicker } from './ProjectContextPicker'
import { useProjectBootstrap } from './project-queries'
import { harnessOptions, SITES_HARNESS_ID } from './project-bootstrap'
import { createChatRequest } from './chat-config'
import { useChatSubmission } from './chat-submission'
import { seedAccepted } from './chat-queries'
import { useSites } from '../sites/site-queries'
import { Link } from 'react-router-dom'

export function ProjectNewChatPage() {
  const { projectId = '' } = useParams()
  const [search] = useSearchParams()
  return <ProjectComposer key={projectId + ':' + search.toString()} projectId={projectId.toLowerCase()} initialHarness={search.get('harness')} initialSite={search.get('site')} />
}

function ProjectComposer({ projectId, initialHarness, initialSite }: { projectId: string; initialHarness: string | null; initialSite: string | null }) {
  const bootstrap = useProjectBootstrap(projectId)
  const [prompt, setPrompt] = useState('')
  const navigate = useNavigate()
  const client = useQueryClient()
  const submission = useChatSubmission('project:' + projectId, reply => {
    if (!reply.session) return
    seedAccepted(client, projectId, reply.session.id, reply)
    navigate('/projects/' + projectId + '/' + reply.session.id)
  })
  const [harnessId, setHarnessId] = useState<string | null>(initialHarness)
  const [siteId, setSiteId] = useState<string | null>(initialSite)
  const [environmentId, setEnvironmentId] = useState<string | null>(null)
  const harnesses = (bootstrap.data?.harnesses ?? []).filter(h => h.enabled)
  const harness = harnessId === null ? harnesses[0] : harnesses.find(h => h.id === harnessId)
  const isSites = harness?.id === SITES_HARNESS_ID
  const sites = useSites(projectId, isSites)
  const siteReady = !isSites || sites.isSuccess && (siteId === null || sites.data.items.some(s => s.id === siteId && s.desired_status === 'ready'))
  const environments = bootstrap.data?.project_environments ?? []
  const environment = environments.find(e => e.id === environmentId) ?? environments[0]
  const options = harnessOptions(bootstrap.data, harness)
  const needsEnvironment = Boolean(harness?.config_schema?.properties?.environment)
  const pending = bootstrap.isPending && !bootstrap.data
  const harnessChoices = harnesses.map(h => ({ id: h.id, triggerLabel: h.id, optionLabel: h.name, searchValue: h.name + ' ' + h.id }))
  const environmentChoices = environments.map(e => ({ id: e.id, triggerLabel: e.name, optionLabel: e.name, searchValue: e.name + ' ' + e.workspace_root + ' ' + e.path, icon: e.type === 'machine' ? ComputerIcon : CloudIcon }))

  return (
    <div className="project-landing-page">
      <div className="project-landing-composer-stack">
        <div className="project-landing-context-row">
          <ProjectContextPicker options={harnessChoices} selectedOptionId={harness?.id ?? null} ariaLabel="Harness" listAriaLabel="Harnesses" searchAriaLabel="Search harnesses" searchPlaceholder="Search harnesses..." loadingLabel="Loading harnesses…" unavailableLabel="No harnesses available" emptyResultsLabel="No harnesses found" isPending={pending} disabled={submission.isPending || submission.uncertain} onSelect={setHarnessId} />
          {needsEnvironment ? <ProjectContextPicker options={environmentChoices} selectedOptionId={environment?.id ?? null} ariaLabel="Environment" listAriaLabel="Project environments" searchAriaLabel="Search environments" searchPlaceholder="Search environments..." loadingLabel="Loading environments…" unavailableLabel="No environments available" emptyResultsLabel="No environments found" isPending={pending} disabled={submission.isPending || submission.uncertain} showSelectedIcon onSelect={setEnvironmentId} /> : null}
          {isSites ? <ProjectContextPicker options={[{ id: 'new', triggerLabel: 'New site', optionLabel: 'New site', searchValue: 'new site' }, ...(sites.data?.items ?? []).filter(s => s.desired_status === 'ready').map(s => ({ id: s.id, triggerLabel: s.name, optionLabel: s.name, searchValue: s.name }))]} selectedOptionId={siteId ?? 'new'} ariaLabel="Site" listAriaLabel="Project sites" searchAriaLabel="Search sites" searchPlaceholder="Search sites..." loadingLabel="Loading sites…" unavailableLabel="Site unavailable" emptyResultsLabel="No sites found" isPending={sites.isPending} disabled={submission.isPending || submission.uncertain} onSelect={id => setSiteId(id === 'new' ? null : id)} /> : null}
        </div>
        <ProjectEnvironmentPromptComposer
          key={harness?.id ?? 'loading'}
          placeholder={isSites ? "Describe what you want to build or investigate" : undefined}
          promptLabel={isSites ? "Site instructions" : undefined}
          promptValue={prompt}
          onPromptChange={setPrompt}
          providerAccounts={options.accounts}
          reasoningLevels={options.reasoningLevels}
          defaultReasoningLevel={options.defaultReasoning}
          webSearchSupported={options.webSearchSupported}
          defaultWebSearchEnabled={options.defaultWebSearch}
          isModelOptionsPending={pending}
          isModelOptionsError={bootstrap.isError && !bootstrap.data}
          onRetryModelOptions={() => void bootstrap.refetch()}
          isSubmissionReady={!submission.uncertain && siteReady && harness !== undefined && (!needsEnvironment || environment !== undefined)}
          isSubmitting={submission.isPending}
          submitError={submission.error?.message}
          onSubmit={value => {
            if (harness) submission.send('/api/projects/' + projectId + '/sessions', createChatRequest(harness, environment, value, siteId))
          }}
        />
        <div className="project-mock-notice">
        {isSites && sites.isError ? <p role="alert">{sites.error.message} <button onClick={() => void sites.refetch()}>Retry</button></p> : null}
        {isSites && sites.isSuccess && !siteReady ? <p role="alert">The selected site is unavailable. Select another site or New site.</p> : null}
        {!pending && !harness ? <p role="alert">This harness is not enabled for the project. <Link to={`/projects/${projectId}/settings`}>Open project settings</Link></p> : null}
        {bootstrap.isError ? <p role={bootstrap.data ? 'status' : 'alert'}>
          {bootstrap.data ? 'Couldn’t refresh options. Showing the last loaded data.' : bootstrap.error.message}
          {' '}<button className="providers-retry-button" type="button" disabled={bootstrap.isFetching} onClick={() => void bootstrap.refetch()}>Retry</button>
        </p> : null}
        {bootstrap.fetchStatus === 'paused' ? <p role="status">You’re offline. Options will refresh when you reconnect.</p> : null}
        {!pending && !bootstrap.isError && harness && options.accounts.length === 0 ? <p role="status">No enabled provider accounts match this harness’s supported models.</p> : null}
        {submission.uncertain ? <p role="alert">The last send is not confirmed. <button className="providers-retry-button" type="button" onClick={submission.retry}>Retry the same send</button></p> : null}
        </div>
      </div>
    </div>
  )
}
