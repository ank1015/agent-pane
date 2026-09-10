// Agent-cell API. These are the only registered inner tools.
type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type SqlValue = null | string | number;
type ImageContent = { type: "image"; mimeType: "image/jpeg"; data: string };
declare const tools: {
  metadata(): Promise<{
    id: string; name: string;
    status: "provisioning" | "ready" | "suspended" | "failed";
    release_id: string | null; created_at: string; updated_at: string;
  }>;
  read(args: {
    file_path: "index.html" | "backend.js"; offset?: number; limit?: number;
  }): Promise<{
    file_path: "index.html" | "backend.js"; revision: string; content: string;
    start_line: number; end_line: number | null; eof: boolean;
    truncation: "line_limit" | "byte_limit" | null; next_offset: number | null;
  }>;
  /** FREEFORM patch string, not JSON. Update edits; Add replaces; Delete resets to
   * blank HTML or a 404 backend. Only index.html/backend.js; no moves. Ordered
   * same-file Delete/Add is supported; only the validated final pair activates.
   * Success returns {}; no patch summary is automatically emitted into exec. */
  apply_patch(patch: string): Promise<Record<string, never>>;
  invoke(args: {
    method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS";
    path: string; query?: Record<string, string>; body?: Json;
    idempotency_key?: string;
  }): Promise<{
    invocation_id: string;
    status: "running" | "succeeded" | "failed" | "timed_out" | "interrupted";
    response: { status: number; body: Json } | null;
    error: { code: string; message: string } | null;
    logs: Array<{ level: "info" | "warn" | "error"; message: string; data: Json }>;
    response_truncated: boolean; logs_truncated: boolean;
  }>;
  /** Async function body inside the site frame: return JSON; no return = null.
   * No page API, Node, imports or code-mode globals. DOM state persists across
   * calls in this run; backend effects are live. Errors may follow effects. */
  browser(args: { action: "evaluate"; code: string }): Promise<{ action: "evaluate"; result: Json }>;
  /** Current 1024x768 viewport, bounded JPEG. Emit with image(result.image). */
  browser(args: { action: "screenshot" }): Promise<{ action: "screenshot"; image: ImageContent }>;
  /** Discard the old page and load the latest release with its backend bridge. */
  browser(args: { action: "reload" }): Promise<{ action: "reload"; release_id: string }>;
  sql(args: { sql: string; params?: SqlValue[]; idempotency_key?: string }): Promise<{
    rows: Array<Record<string, SqlValue>>; changes: number; read_only: boolean;
  }>;
};
// Rejected tool calls throw Error with code:string, message:string, uncertain:boolean.
