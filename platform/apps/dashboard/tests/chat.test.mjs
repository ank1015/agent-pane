import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient } from '@tanstack/react-query'
import { createServer } from 'vite'
const vite = await createServer({ server: { middlewareMode: true, ws: false }, appType: 'custom' })
after(() => vite.close())
const { buildConversation } = await vite.ssrLoadModule('/src/features/projects/project-conversation.ts')
const { createChatRequest, lockedChatOptions, sessionReasoningLevels } = await vite.ssrLoadModule('/src/features/projects/chat-config.ts')
const { chatKeys, chatMessagesOptions, chatRunsOptions, retryRead, readCursorPages, seedAccepted } = await vite.ssrLoadModule('/src/features/projects/chat-queries.ts')
const { postJson, ApiError } = await vite.ssrLoadModule('/src/lib/api-client.ts')
const { RUN_EVENT_TYPES } = await vite.ssrLoadModule('/src/features/projects/chat-stream.ts')
const { ProjectEnvironmentPromptComposer } = await vite.ssrLoadModule('/src/features/projects/ProjectEnvironmentPromptComposer.tsx')
const id = '01900000-0000-7000-8000-000000000001'
const user = { role: 'user', id: 'user-1', timestamp: 0, content: [{ type: 'text', content: 'hello' }] }
const run = { id: 'run', status: 'ready', created_at: '2026-09-05T00:00:00Z', final_message_id: null }
const input = { id: 'input', run_id: 'run', status: 'pending', payload: { message: user }, handling: null, created_at: run.created_at }
const history = { message_id: 'history', revision: 1, run_id: 'run', message: user, created_at: run.created_at }

test('locked reasoning retains the harness scale instead of a single selected bar', () => {
  const levels = ['low', 'medium', 'high', 'xhigh', 'max']
  const harness = { config_schema: { properties: { reasoning_level: { enum: levels } } } }
  for (const [index, selected] of levels.entries()) {
    const scale = sessionReasoningLevels(harness, selected)
    assert.deepEqual(scale, levels)
    assert.equal(scale.indexOf(selected) + 1, index + 1)
    assert.deepEqual(sessionReasoningLevels(undefined, selected), levels)
  }
  assert.deepEqual(sessionReasoningLevels({ config_schema: null }, 'high'), levels)
  assert.deepEqual(sessionReasoningLevels({ config_schema: { properties: { reasoning_level: { enum: ['low', 'high'] } } } }, 'high'), ['low', 'high'])
  assert.deepEqual(sessionReasoningLevels(undefined, 'custom'), ['custom'])
  assert.deepEqual(sessionReasoningLevels(harness, undefined), [])
})

test('High renders three active bars in the read-only chat composer', () => {
  const html = renderToStaticMarkup(createElement(ProjectEnvironmentPromptComposer, {
    providerAccounts: [], reasoningLevels: sessionReasoningLevels(undefined, 'high'),
    lockedOptions: { accountId: id, provider: 'openai', modelId: 'gpt-5.6-terra', reasoningLevel: 'high', webSearchEnabled: false },
    optionsReadOnly: true,
  }))
  assert.match(html, /aria-label="Reasoning: High"/)
  assert.equal((html.match(/class="project-environment-reasoning-bar--active"/g) ?? []).length, 3)
  assert.match(html, /project-environment-reasoning-toggle--static/)
  assert.doesNotMatch(html, /Click to use|project-environment-reasoning-label--animate/)
})

test('live updates subscribe to platform start/resume and scheduling lifecycle events', () => {
  for (const name of ['run.started', 'run.resumed', 'run.yielded', 'run.wait_resolved']) {
    assert.ok(RUN_EVENT_TYPES.includes(name), name)
  }
  for (const name of ['run.claimed', 'run.ready', 'run.released']) {
    assert.ok(!RUN_EVENT_TYPES.includes(name), name)
  }
  assert.equal(new Set(RUN_EVENT_TYPES).size, RUN_EVENT_TYPES.length)
})

test('accepted inputs render before history and reconcile without duplication', () => {
  const pending = buildConversation([], [run], [input])
  assert.equal(pending[0].message.deliveryLabel, undefined)
  assert.equal(pending[0].message.message.id, user.id)
  assert.equal(pending[1].kind, 'run-progress')
  for (const status of ['ready', 'running', 'waiting']) {
    assert.equal(buildConversation([], [{ ...run, status }], [input])[0].message.deliveryLabel, undefined)
  }
  for (const handling of [null, { message_id: 'history' }]) {
    const items = buildConversation([history], [run], [{ ...input, handling }])
    assert.equal(items.filter(i => i.kind === 'message').length, 1)
    assert.equal(items[0].message.message_id, 'history')
  }
})
test('terminal runs retain unconsumed and rejected inputs', () => {
  for (const status of ['failed', 'aborted']) {
    const items = buildConversation([], [{ ...run, status }], [input])
    assert.equal(items[0].message.deliveryLabel, 'Not consumed by this run')
    assert.equal(items.some(i => i.kind === 'run-error'), status === 'failed')
  }
  assert.equal(buildConversation([], [run], [{ ...input, status: 'rejected' }])[0].message.deliveryLabel, 'Not delivered')
})
test('main transcript includes final replies, not intermediate tool-loop responses', () => {
  const assistant = { role: 'assistant', id: 'answer', content: [{ type: 'response', response: { content: 'Done' } }] }
  const messages = [history, { ...history, message_id: 'step', revision: 2, message: assistant }, { ...history, message_id: 'final', revision: 3, message: assistant }]
  assert.deepEqual(buildConversation(messages, [{ ...run, status: 'completed', final_message_id: 'final' }], [input]).map(i => i.id), ['history', 'progress:run', 'final'])
  assert.equal(buildConversation([{ ...messages[2], run_id: null }], [], []).length, 1)
})
test('creation freezes selected config, resolves machine/snapshot shape and passes the native workspace path', () => {
  const submission = { prompt: 'hello', accountId: id, provider: 'openai', modelId: 'model', reasoningLevel: 'high', webSearchEnabled: true }
  const harness = { id: 'basic', config_schema: { properties: { environment: { type: 'object' } } } }
  const env = { type: 'machine', machine_id: id, workspace_root: '/Users/example/Desktop', path: 'test' }
  const request = createChatRequest(harness, env, submission)
  assert.equal(request.config_override.environment.workspace_root, '/Users/example/Desktop')
  assert.equal(request.config_override.environment.path, 'test')
  assert.equal(request.config_override.web_search_enabled, undefined)
  assert.equal(request.initial_run.config_override, undefined)
  assert.equal(request.initial_run.expected_session_revision, 0)
  assert.equal(request.initial_run.input.role, 'user')
  assert.equal(lockedChatOptions(request.config_override).modelId, 'model')
  const sandbox = createChatRequest(harness, { ...env, type: 'sandbox', snapshot_id: id }, submission).config_override.environment
  assert.equal(sandbox.snapshot_id, id)
  assert.equal(sandbox.machine_id, undefined)
  assert.equal(createChatRequest({ id: 'environments', config_schema: {} }, undefined, submission).config_override.environment, undefined)
  assert.throws(() => createChatRequest(harness, undefined, submission), /Select an environment/)
})
test('new session titles use harness-specific prefixes without changing the prompt', () => {
  const submission = { prompt: '  Set up a Python workspace  ', accountId: id, provider: 'openai', modelId: 'model', reasoningLevel: 'high', webSearchEnabled: false }
  const environmentRequest = createChatRequest({ id: 'environments', config_schema: {} }, undefined, submission)
  assert.equal(environmentRequest.title, '(Env) Set up a Python workspace')
  for (const harnessId of ['basic-cc-tools-harness', 'another-harness']) {
    const request = createChatRequest({ id: harnessId, config_schema: {} }, undefined, submission)
    assert.equal(request.title, 'Set up a Python workspace')
    assert.deepEqual(request.initial_run.input.content, environmentRequest.initial_run.input.content)
  }
  const long = createChatRequest({ id: 'environments', config_schema: {} }, undefined, { ...submission, prompt: 'x'.repeat(100) })
  assert.equal(long.title, '(Env) ' + 'x'.repeat(74))
})

test('incremental history follows all revision pages and preserves cached messages', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity } } })
  client.setQueryData(chatKeys.messages(id), [history])
  const original = globalThis.fetch
  const calls = []
  globalThis.fetch = async (url, init) => {
    calls.push(url)
    assert.ok(init.signal instanceof AbortSignal)
    return { ok: true, json: async () => calls.length === 1 ? { items: [{ ...history, message_id: 'two', revision: 2 }], next_after_revision: 2 } : { items: [{ ...history, message_id: 'three', revision: 3 }], next_after_revision: null } }
  }
  try {
    const messages = await chatMessagesOptions(client, id, true).queryFn({ signal: new AbortController().signal })
    assert.deepEqual(messages.map(m => m.revision), [1, 2, 3])
    assert.ok(calls[0].endsWith('after_revision=1'))
    assert.ok(calls[1].endsWith('after_revision=2'))
  } finally { globalThis.fetch = original; client.clear() }
})
test('cursor loops fail safely; retries and background polling are bounded', async () => {
  const original = globalThis.fetch
  globalThis.fetch = async () => ({ ok: true, json: async () => ({ items: [], next_cursor: 'repeat' }) })
  try { await assert.rejects(readCursorPages('/api/test', new AbortController().signal), /repeated/) } finally { globalThis.fetch = original }
  assert.equal(retryRead(0, new ApiError(409, 'Conflict')), false)
  assert.equal(retryRead(0, new ApiError(503, 'Unavailable')), true)
  assert.equal(retryRead(2, new Error('network')), false)
  assert.equal(chatRunsOptions(id, true).refetchInterval, 5000)
  assert.equal(chatRunsOptions(id, false).refetchInterval, 30000)
  assert.equal(chatRunsOptions(id, true).refetchIntervalInBackground, false)
})
test('idempotent POST transmits the exact stable body and key', async () => {
  const original = globalThis.fetch
  let seen
  globalThis.fetch = async (url, init) => { seen = { url, init }; return { ok: true, json: async () => ({ ok: true }) } }
  try {
    const body = { input: user, expected_session_revision: 5 }
    await postJson('/api/sessions/' + id + '/runs', body, 'same-key')
    assert.equal(seen.init.headers['Idempotency-Key'], 'same-key')
    assert.equal(seen.init.body, JSON.stringify(body))
  } finally { globalThis.fetch = original }
})
test('mutation seeding never pretends an unloaded existing history is complete', () => {
  const client = new QueryClient()
  seedAccepted(client, id, id, { run, input })
  assert.equal(client.getQueryData(chatKeys.messages(id)), undefined)
  assert.equal(client.getQueryData(chatKeys.runs(id)), undefined)
  client.clear()
})

test('Sites freezes an optional site and needs no authoring environment', () => {
  const harness = { id: 'sites', config_schema: { properties: { siteId: { type: ['string', 'null'] } } } }
  const submission = { prompt: 'Build a results site', accountId: id, provider: 'chatgpt', modelId: 'gpt-5.6-luna', reasoningLevel: 'low' }
  const fresh = createChatRequest(harness, undefined, submission)
  assert.equal(fresh.config_override.siteId, null)
  assert.equal(fresh.config_override.environment, undefined)
  assert.equal(fresh.title, '(Sites) Build a results site')
  const existing = createChatRequest(harness, { type: 'sandbox', snapshot_id: id }, submission, id)
  assert.equal(existing.config_override.siteId, id)
  assert.equal(existing.config_override.environment, undefined)
  assert.equal(existing.config_override.account_id, id)
  assert.throws(() => createChatRequest(harness, undefined, submission, 'bad-site'), /site/i)
  for (const event of ['code_mode.journal', 'run.output_published']) assert.ok(RUN_EVENT_TYPES.includes(event))
})
