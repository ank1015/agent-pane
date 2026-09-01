export type JsonObject = Record<string, unknown>

export type RunStatus =
  | 'active'
  | 'waiting'
  | 'aborted'
  | 'completed'
  | 'failed'

export type AgentSession = {
  session_id: string
  current_revision: number
  created_at: string
}

export type TextContent = {
  type: 'text'
  content: string
  metadata?: JsonObject
}

export type ImageContent = {
  source:
    | { type: 'url'; url: string }
    | { type: 'base64'; data: string; mime_type: string }
  detail?: 'auto' | 'low' | 'high' | 'original'
  metadata?: JsonObject
}

export type UserMessage = {
  role: 'user'
  id: string
  timestamp: number
  content: Array<TextContent | ({ type: 'image' } & ImageContent)>
}

export type AssistantContent =
  | { type: 'response'; response: Omit<TextContent, 'type'> }
  | { type: 'thinking'; thinking_text: string }
  | {
      type: 'tool_call'
      name: string
      arguments: JsonObject | string
      tool_call_id: string
    }

export type AssistantMessage = {
  role: 'assistant'
  id: string
  model: { provider: string; id: string; name?: string }
  usage?: JsonObject
  duration_ms: number
  native_message: unknown
  content: AssistantContent[]
  stop_reason:
    | 'stop'
    | 'length'
    | 'tool_use'
    | 'refusal'
    | 'content_filter'
    | 'pause_turn'
  timestamp: number
}

export type SessionMessageBody =
  | UserMessage
  | AssistantMessage
  | ({ role: 'system' | 'tool_result' | 'custom' } & JsonObject)

export type SessionMessage = {
  session_message_id: string
  session_id: string
  revision: number
  message: SessionMessageBody
  origin: 'external' | 'harness' | 'system' | 'imported'
  delivery: 'immediate' | 'next_turn'
  run_id: string | null
  turn_number: number | null
  created_at: string
  committed_at: string
}

export type SessionMessagePage = {
  items: SessionMessage[]
  next_after_revision: number | null
}

export type ProjectRun = {
  run_id: string
  session_id: string
  trigger_message_id: string
  harness_revision_id: string
  resolved_config: JsonObject
  status: RunStatus
  current_turn: number
  max_turns: number
  state_version: number
  final_message_id: string | null
  failure: unknown | null
  created_at: string
  activated_at: string
  finished_at: string | null
}

export type ProjectRunSummary = Omit<ProjectRun, 'resolved_config'>

export type ProjectRunPage = {
  items: ProjectRunSummary[]
  next_cursor: string | null
}

export type ProjectHarnessSession = {
  session: AgentSession
  harness_id: string
  latest_run: ProjectRun | null
  active_run: ProjectRun | null
}

export type RunEventType =
  | 'run_started'
  | 'turn_requested'
  | 'turn_started'
  | 'turn_ended'
  | 'run_waiting'
  | 'run_resumed'
  | 'run_completed'
  | 'run_failed'
  | 'run_aborted'
  | 'progress'

export type RunEvent = {
  event_id: string
  run_id: string
  sequence: number
  turn_number: number | null
  state_version: number
  run_status: RunStatus
  source: 'agent' | 'harness'
  occurred_at: string
  recorded_at: string
  type: RunEventType
  details?: JsonObject
}

export type RunEventPage = {
  items: RunEvent[]
  next_after_sequence: number | null
}

export type RunAbort = {
  abort_id: string
  run_id: string
  turn_number: number
  reason: string | null
  payload: JsonObject
  requested_at: string
}

export type RunAbortResult = {
  abort: RunAbort
  run: ProjectRun
}

export type ProjectRunLimits = {
  max_turns?: number
}

export type CreateProjectHarnessSessionRequest = {
  harness_id: string
  prompt: string
  attachments?: ImageContent[]
  config_override?: JsonObject
  limits?: ProjectRunLimits
}

export type CreateProjectHarnessSessionResponse = {
  session: AgentSession
  trigger_message: SessionMessage
  run: ProjectRun
}

export type StartProjectHarnessRunRequest = {
  prompt: string
  attachments?: ImageContent[]
  config_override?: JsonObject
  limits?: ProjectRunLimits
  expected_session_revision: number
}

export type StartProjectHarnessRunResponse = {
  trigger_message: SessionMessage
  run: ProjectRun
}

export type AbortProjectHarnessRunRequest = {
  abort_id: string
  expected_state_version: number
  reason: string | null
  payload?: JsonObject
}

export function isTerminalRunStatus(status: RunStatus) {
  return status === 'completed' || status === 'failed' || status === 'aborted'
}

export function isLiveRunStatus(status: RunStatus) {
  return status === 'active' || status === 'waiting'
}
