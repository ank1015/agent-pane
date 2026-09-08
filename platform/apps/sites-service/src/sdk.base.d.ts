/** Backend SDK v1. Bundle dependencies into backend.js; no Node/browser host APIs. */
export type SqlValue = null | string | number;
export type Row = Record<string, SqlValue>;
export interface Statement { sql: string; params?: SqlValue[] }
export interface WriteResult { changes: number; rows: Row[] }
export interface Transaction {
  query(sql: string, params?: SqlValue[]): Promise<Row[]>;
  /** Data writes only (INSERT/UPDATE/DELETE). Create/alter schema with the agent authoring SDK before activating a handler. */
  execute(sql: string, params?: SqlValue[]): Promise<WriteResult>;
}
export interface Database extends Transaction {
  batch(statements: Statement[]): Promise<WriteResult[]>;
  transaction<T>(fn: (tx: Transaction) => Promise<T>): Promise<T>;
}
/** Completion callback POST body. Application callback payload is nested in payload. */
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
  readonly platform: SitePlatform;
  readonly log: Readonly<{
    info(message: string, data?: Json): void;
    warn(message: string, data?: Json): void;
    error(message: string, data?: Json): void;
  }>;
}
export type Handler = (request: Request, ctx: Context) => Response | Promise<Response>;


