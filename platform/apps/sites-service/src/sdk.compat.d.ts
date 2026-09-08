/** Legacy backend aliases retained during migration. */
export type PlatformRecord = { [key: string]: Json };
export interface Page { items: PlatformRecord[]; next_cursor: string | null }
export type UserMessage = UserInput;
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

export interface SitePlatform extends Omit<Platform, 'sessions'> {
  readonly sessions: Platform['sessions'] & Readonly<{
    startOptions(harnessId: string): Promise<{
      harnessId: string; environmentRequired: boolean;
      accounts: { accountId: string; name: string; provider: string; modelIds: string[] }[];
      reasoningLevels: string[] | null; defaultReasoning: string | null;
      webSearchSupported: boolean; defaultWebSearch: boolean | null;
    }>;
    start(input: StartSession, options: MutationOptions): Promise<AcceptedSession>;
    metrics(sessionId: string, options?: { runId?: string }): Promise<PlatformRecord>;
    send(sessionId: string, input: { prompt: string; expectedRevision: number; expectedRunId: string | null; onComplete?: CompletionCallback }, options: MutationOptions): Promise<AcceptedSession>;
    stop(sessionId: string, options: MutationOptions & { expectedRunId: string; reason?: string }): Promise<AcceptedSession>;
  }>;
  readonly callbacks: Readonly<{
    list(options?: { limit?: number; after?: string }): Promise<{ items: PlatformRecord[]; next_after: string | null }>;
    get(callbackId: string): Promise<PlatformRecord>;
  }>;
}
