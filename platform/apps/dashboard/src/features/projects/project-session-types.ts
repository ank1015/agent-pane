export type JsonObject = Record<string, unknown>

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

export type MessageContentPart =
  | TextContent
  | ({ type: 'image' } & ImageContent)

export type UserMessage = {
  role: 'user'
  id: string
  timestamp: number
  content: MessageContentPart[]
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

export type AssistantUsageCost = {
  input?: number
  output?: number
  cache_read?: number
  cache_write?: number
  total: number
}

export type AssistantUsage = {
  input?: number
  output?: number
  cache_read?: number
  cache_write?: number
  cost?: AssistantUsageCost
}

export type AssistantMessage = {
  role: 'assistant'
  id: string
  model: { provider: string; id: string; name?: string }
  usage?: AssistantUsage
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

export type SystemMessage = {
  role: 'system'
  id: string
  timestamp: number
  content: TextContent[]
}

export type ToolResultMessage = {
  role: 'tool_result'
  id: string
  tool_name: string
  tool_call_id: string
  content: MessageContentPart[]
  details?: unknown
  timestamp: number
  outcome:
    | { status: 'success' }
    | {
        status: 'error'
        error: { message: string; name?: string }
      }
}

export type CustomMessage = {
  role: 'custom'
  id: string
  content: JsonObject
  tag?: string
  timestamp: number
}

export type SessionMessageBody =
  | UserMessage
  | AssistantMessage
  | SystemMessage
  | ToolResultMessage
  | CustomMessage

export type RunStatus = 'ready' | 'running' | 'waiting' | 'completed' | 'failed' | 'aborted'
export type ProjectRunSummary = {
  id: string; session_id: string; project_id: string; config: JsonObject; status: RunStatus
  version: number; abort_requested_at: string | null; final_message_id: string | null
  error: JsonObject | null; last_event_sequence: number
  created_at: string; started_at: string | null; finished_at: string | null
}
export type SessionMessage = {
  message_id: string; revision: number; run_id: string | null; origin_run_id: string | null
  message: SessionMessageBody; created_at: string
  deliveryLabel?: string
}
export type ChatSession = {
  id: string; project_id: string; harness_id: string; title: string | null
  config: JsonObject; current_revision: number; active_run: ProjectRunSummary | null
  archived_at: string | null
}
export type RunInput = {
  id: string; run_id: string; kind: string; payload: { message?: UserMessage }
  status: 'pending' | 'handled' | 'rejected'
  handling: { message_id?: string | null; reason?: string } | null
  created_at: string
}
export type RunEvent = { id: string; run_id: string; sequence: number; type: string; payload: JsonObject }
export type StartRunRequest = { input: UserMessage; expected_session_revision: number }
export type CreateSessionRequest = {
  harness_id: string; title: string; config_override: JsonObject; initial_run: StartRunRequest
}
export type AcceptedRun = { session?: ChatSession; run: ProjectRunSummary; input: RunInput }
export const isLiveRunStatus = (status: RunStatus) => status === 'ready' || status === 'running' || status === 'waiting'
