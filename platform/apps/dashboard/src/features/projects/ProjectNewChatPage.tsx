import { useState } from 'react'
import { useParams } from 'react-router-dom'
import { CloudIcon, ComputerIcon } from '@hugeicons/core-free-icons'
import { ProjectEnvironmentPromptComposer } from './ProjectEnvironmentPromptComposer'
import { ProjectContextPicker } from './ProjectContextPicker'
import { useProjectBootstrap } from './project-queries'
import { ENVIRONMENTS_HARNESS_ID, harnessOptions } from './project-bootstrap'

export function ProjectNewChatPage() {
  const { projectId = '' } = useParams()
  return <ProjectComposer key={projectId} projectId={projectId} />
}

function ProjectComposer({ projectId }: { projectId: string }) {
  const bootstrap = useProjectBootstrap(projectId)
  const [prompt, setPrompt] = useState('')
  const [previewSubmitted, setPreviewSubmitted] = useState(false)
  const [harnessId, setHarnessId] = useState<string | null>(null)
  const [environmentId, setEnvironmentId] = useState<string | null>(null)
  const harnesses = (bootstrap.data?.harnesses ?? []).filter(h => h.enabled)
  const harness = harnesses.find(h => h.id === harnessId) ?? harnesses[0]
  const environments = bootstrap.data?.project_environments ?? []
  const environment = environments.find(e => e.id === environmentId) ?? environments[0]
  const options = harnessOptions(bootstrap.data, harness)
  const needsEnvironment = harness !== undefined && harness.id !== ENVIRONMENTS_HARNESS_ID
  const pending = bootstrap.isPending && !bootstrap.data
  const harnessChoices = harnesses.map(h => ({ id: h.id, triggerLabel: h.id, optionLabel: h.name, searchValue: h.name + ' ' + h.id }))
  const environmentChoices = environments.map(e => ({ id: e.id, triggerLabel: e.name, optionLabel: e.name, searchValue: e.name + ' ' + (e.workspace_root_path ?? '') + ' ' + e.path, icon: e.type === 'machine' ? ComputerIcon : CloudIcon }))

  return (
    <div className="project-landing-page">
      <div className="project-landing-composer-stack">
        <div className="project-landing-context-row">
          <ProjectContextPicker options={harnessChoices} selectedOptionId={harness?.id ?? null} ariaLabel="Harness" listAriaLabel="Harnesses" searchAriaLabel="Search harnesses" searchPlaceholder="Search harnesses..." loadingLabel="Loading harnesses…" unavailableLabel="No harnesses available" emptyResultsLabel="No harnesses found" isPending={pending} onSelect={id => { setHarnessId(id); setPreviewSubmitted(false) }} />
          {needsEnvironment ? <ProjectContextPicker options={environmentChoices} selectedOptionId={environment?.id ?? null} ariaLabel="Environment" listAriaLabel="Project environments" searchAriaLabel="Search environments" searchPlaceholder="Search environments..." loadingLabel="Loading environments…" unavailableLabel="No environments available" emptyResultsLabel="No environments found" isPending={pending} showSelectedIcon onSelect={setEnvironmentId} /> : null}
        </div>
        <ProjectEnvironmentPromptComposer
          key={harness?.id ?? 'loading'}
          promptValue={prompt}
          onPromptChange={value => { setPrompt(value); setPreviewSubmitted(false) }}
          providerAccounts={options.accounts}
          reasoningLevels={options.reasoningLevels}
          defaultReasoningLevel={options.defaultReasoning}
          webSearchSupported={options.webSearchSupported}
          defaultWebSearchEnabled={options.defaultWebSearch}
          isModelOptionsPending={pending}
          isModelOptionsError={bootstrap.isError && !bootstrap.data}
          onRetryModelOptions={() => void bootstrap.refetch()}
          isSubmissionReady={harness !== undefined && (!needsEnvironment || environment !== undefined)}
          onSubmit={() => setPreviewSubmitted(true)}
        />
        <div className="project-mock-notice">
        {bootstrap.isError ? <p role={bootstrap.data ? 'status' : 'alert'}>
          {bootstrap.data ? 'Couldn’t refresh options. Showing the last loaded data.' : bootstrap.error.message}
          {' '}<button className="providers-retry-button" type="button" disabled={bootstrap.isFetching} onClick={() => void bootstrap.refetch()}>Retry</button>
        </p> : null}
        {bootstrap.fetchStatus === 'paused' ? <p role="status">You’re offline. Options will refresh when you reconnect.</p> : null}
        {!pending && !bootstrap.isError && harness && options.accounts.length === 0 ? <p role="status">No enabled provider accounts match this harness’s supported models.</p> : null}
        {previewSubmitted ? <p role="status">Options are live; starting a run is not wired yet.</p> : null}
        </div>
      </div>
    </div>
  )
}
