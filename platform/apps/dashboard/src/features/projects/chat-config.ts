import { ENVIRONMENTS_HARNESS_ID, SITES_HARNESS_ID, type Harness } from './project-bootstrap'
import type { ProjectEnvironment } from './project-types'
import type { ProjectEnvironmentPromptSubmission, ProjectEnvironmentPromptLockedOptions } from './ProjectEnvironmentPromptComposer'
import type { CreateSessionRequest, JsonObject } from './project-session-types'
import { userMessage } from './chat-submission'

const DEFAULT_REASONING_LEVELS = ['low', 'medium', 'high', 'xhigh', 'max'] as const

// Locking a session fixes its selection, not the scale used to display it.
export function sessionReasoningLevels(harness: Harness | undefined, selected: string | undefined): readonly string[] {
  if (!selected) return []
  const levels = (harness?.config_schema?.properties?.reasoning_level?.enum ?? [])
    .filter((value): value is string => typeof value === 'string')
  if (levels.includes(selected)) return levels
  // Keep a meaningful indicator while bootstrap loads, or for an older harness
  // registration. This fallback is display-only; it never changes session config.
  if (DEFAULT_REASONING_LEVELS.some(level => level === selected)) return DEFAULT_REASONING_LEVELS
  return [...levels, selected]
}

export function createChatRequest(harness: Harness, environment: ProjectEnvironment | undefined, submission: ProjectEnvironmentPromptSubmission, siteId: string | null = null): CreateSessionRequest {
  const config: JsonObject = {
    model: { provider: submission.provider, id: submission.modelId },
    account_id: submission.accountId,
    reasoning_level: submission.reasoningLevel,
  }
  if (harness.config_schema?.properties?.web_search_enabled?.type === 'boolean') config.web_search_enabled = submission.webSearchEnabled
  if (harness.config_schema?.properties?.environment) {
    if (!environment) throw new Error('Select an environment.')
    const sourceId = environment.type === 'machine' ? environment.machine_id : environment.snapshot_id
    if (!sourceId) throw new Error('This environment is missing its machine or snapshot.')
    config.environment = { type: environment.type, [environment.type === 'machine' ? 'machine_id' : 'snapshot_id']: sourceId, workspace_root: environment.workspace_root, path: environment.path }
  }
  if (harness.id === SITES_HARNESS_ID) {
    if (siteId !== null && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(siteId)) throw new Error('Select a valid site.')
    config.siteId = siteId
  }
  const titlePrefix = harness.id === ENVIRONMENTS_HARNESS_ID ? '(Env) ' : harness.id === SITES_HARNESS_ID ? '(Sites) ' : ''
  const title = (titlePrefix + submission.prompt.trim()).slice(0, 80).trimEnd()
  return { harness_id: harness.id, title, config_override: config, initial_run: { expected_session_revision: 0, input: userMessage(submission.prompt) } }
}

export function lockedChatOptions(config: JsonObject): ProjectEnvironmentPromptLockedOptions | null {
  const model = config.model as { provider?: string; id?: string } | undefined
  if (!model || !['openai', 'chatgpt', 'fireworks'].includes(model.provider ?? '') ||
    typeof model.id !== 'string' || typeof config.account_id !== 'string' || typeof config.reasoning_level !== 'string') return null
  return { accountId: config.account_id, provider: model.provider as ProjectEnvironmentPromptLockedOptions['provider'], modelId: model.id, reasoningLevel: config.reasoning_level, webSearchEnabled: config.web_search_enabled === true }
}
