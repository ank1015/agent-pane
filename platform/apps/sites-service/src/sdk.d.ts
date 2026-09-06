/** Backend SDK v1. Bundle dependencies into backend.js; no Node/browser host APIs. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type SqlValue = null | string | number;
export type Row = Record<string, SqlValue>;
export interface Statement { sql: string; params?: SqlValue[] }
export interface WriteResult { changes: number; rows: Row[] }
export interface Transaction {
  query(sql: string, params?: SqlValue[]): Promise<Row[]>;
  execute(sql: string, params?: SqlValue[]): Promise<WriteResult>;
}
export interface Database extends Transaction {
  batch(statements: Statement[]): Promise<WriteResult[]>;
  transaction<T>(fn: (tx: Transaction) => Promise<T>): Promise<T>;
}
export interface Request {
  method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'HEAD' | 'OPTIONS';
  path: string;
  query: Record<string, string>;
  body: Json;
}
export interface Response { status: number; body: Json }
export interface Context {
  readonly site: Readonly<{ id: string; projectId: string; releaseId: string; sdkVersion: '1' }>;
  readonly invocation: Readonly<{
    id: string;
    source: 'internal' | 'callback';
    eventId: string | null;
    subscriptionId: string | null;
    /** Unix milliseconds. The host enforces a separate monotonic deadline. */
    deadlineAt: number;
    throwIfCancelled(): void;
  }>;
  readonly db: Database;
  readonly platform: Platform;
  readonly log: Readonly<{
    info(message: string, data?: Json): void;
    warn(message: string, data?: Json): void;
    error(message: string, data?: Json): void;
  }>;
}
export type Handler = (request: Request, ctx: Context) => Response | Promise<Response>;


/** Platform-owned records retain their existing snake_case field names. */
export type PlatformRecord = { [key: string]: Json };
export interface Page { items: PlatformRecord[]; next_cursor: string | null }
export interface MutationOptions { idempotencyKey: string }
export interface CompletionCallback { path: string; payload?: Json }
export interface CompletionEvent {
  id: string;
  type: 'run.completed' | 'run.failed' | 'run.aborted';
  subscriptionId: string;
  sessionId: string;
  runId: string;
  status: 'completed' | 'failed' | 'aborted';
  finishedAt: string;
  payload: Json;
}
export interface StartSession {
  harnessId: string;
  environmentId?: string;
  prompt: string;
  title?: string;
  model: { provider: string; id: string };
  accountId: string;
  onComplete?: CompletionCallback;
  options?: { reasoningLevel?: string; webSearchEnabled?: boolean };
}
export interface AcceptedSession {
  sessionId: string;
  runId: string;
  session: PlatformRecord;
  run: PlatformRecord;
  input: PlatformRecord;
  callback?: { subscriptionId: string };
}
export interface Platform {
  readonly callbacks: Readonly<{
    list(options?: { limit?: number; after?: string }): Promise<{ items: PlatformRecord[]; next_after: string | null }>;
    get(callbackId: string): Promise<PlatformRecord>;
  }>;
  readonly environments: Readonly<{
    list(): Promise<{ items: PlatformRecord[] }>;
    get(environmentId: string): Promise<PlatformRecord>;
  }>;
  readonly harnesses: Readonly<{
    list(): Promise<{ items: PlatformRecord[] }>;
    get(harnessId: string): Promise<PlatformRecord>;
  }>;
  readonly sessions: Readonly<{
    startOptions(harnessId: string): Promise<{
      harnessId: string;
      environmentRequired: boolean;
      accounts: { accountId: string; name: string; provider: string; modelIds: string[] }[];
      reasoningLevels: string[] | null;
      defaultReasoning: string | null;
      webSearchSupported: boolean;
      defaultWebSearch: boolean | null;
    }>;
    start(input: StartSession, options: MutationOptions): Promise<AcceptedSession>;
    list(options?: { limit?: number; cursor?: string }): Promise<Page>;
    get(sessionId: string): Promise<PlatformRecord>;
    messages(sessionId: string, options?: { limit?: number; afterRevision?: number; runId?: string }): Promise<{ items: PlatformRecord[]; next_after_revision: number | null }>;
    metrics(sessionId: string, options?: { runId?: string }): Promise<PlatformRecord>;
    send(sessionId: string, input: { prompt: string; expectedRevision: number; expectedRunId: string | null; onComplete?: CompletionCallback }, options: MutationOptions): Promise<AcceptedSession>;
    stop(sessionId: string, options: MutationOptions & { expectedRunId: string; reason?: string }): Promise<AcceptedSession>;
  }>;
}
