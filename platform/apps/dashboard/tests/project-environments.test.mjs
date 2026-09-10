import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router-dom'
import { createTestViteServer } from './vite-test-server.mjs'

const vite = await createTestViteServer()
after(() => vite.close())
const { ProjectEnvironmentsTable } = await vite.ssrLoadModule('/src/features/projects/ProjectEnvironmentsTable.tsx')
const { default: App } = await vite.ssrLoadModule('/src/App.tsx')
const { projectKeys } = await vite.ssrLoadModule('/src/features/projects/project-queries.ts')
const { harnessOptions } = await vite.ssrLoadModule('/src/features/projects/project-bootstrap.ts')
const { projectBootstrapOptions } = await vite.ssrLoadModule('/src/features/projects/project-queries.ts')
const { ApiError } = await vite.ssrLoadModule('/src/lib/api-client.ts')
const basicHarness = { id: 'basic-cc-tools-harness', name: 'Basic CC Tools Harness', enabled: true, supported_models: { openai: ['gpt-5.6-sol'] }, default_config: { reasoning_level: 'high' }, config_schema: { properties: { environment: { type: 'object' }, reasoning_level: { enum: ['low', 'high'] } } } }
const bootstrap = { harnesses: [basicHarness], provider_accounts: [{ id: 'account', name: 'Personal', provider: 'openai', status: 'enabled' }], project_environments: [] }
const projectId = '01900000-0000-7000-8000-000000000001'
const otherId = '01900000-0000-7000-8000-000000000002'
const record = { id: '01900000-0000-7000-8000-000000000003', project_id: projectId, name: 'Local workspace', type: 'machine', machine_id: otherId, snapshot_id: null, workspace_root: '/Users/test/workspace', path: 'src/app', created_at: '2026-09-04T00:00:00Z', updated_at: '2026-09-04T01:00:00Z' }
function table(overrides = {}) {
  return renderToStaticMarkup(createElement(ProjectEnvironmentsTable, { query: { isPending: false, isError: false, isFetching: false, data: [], refetch: () => {}, ...overrides } }))
}
function app(path, environments = [], bootstrapData = bootstrap) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } })
  client.setQueryData(projectKeys.list(), [{ id: projectId, name: 'Example project', avatar: null }])
  client.setQueryData(projectKeys.environments(projectId), environments)
  client.setQueryData(projectKeys.bootstrap(projectId), { ...bootstrapData, project_environments: environments })
  try {
    return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, { initialEntries: [path] }, createElement(App))))
  } finally { client.clear() }
}

test('project settings is a bottom sidebar tab with an empty heading-only page', () => {
  const html = app(`/projects/${projectId}/settings`)
  assert.match(html, /project-sidebar-footer/)
  assert.match(html, /aria-label="Project Settings"[^>]*aria-current="page"/)
  assert.match(html, /<h1 class="cursor-page-title">Settings<\/h1>/)
  assert.ok(!html.includes('Environment instructions'))
})
test('project cards use real project detail links', () => {
  assert.match(app('/projects'), new RegExp(`href="/projects/${projectId}"`))
})
test('project New Chat uses live cached landing options', () => {
  const html = app(`/projects/${projectId}`, [record])
  for (const value of ['project-landing-page', 'project-landing-context-row', 'Harness: basic-cc-tools-harness', 'Environment: Local workspace', 'Describe the Environment', 'Model: GPT-5.6 Sol', 'Reasoning: High', 'Back to Dashboard']) {
    assert.ok(html.includes(value), value)
  }
  assert.match(html, /aria-label="New Chat"[^>]*aria-current="page"/)
  assert.match(html, /aria-label="Send prompt"[^>]*disabled/)
  assert.ok(!html.includes('Configure Environments'))
  assert.ok(!html.includes('Prompt settings'))
  assert.ok(!html.includes('<header'))
})

test('harness options intersect enabled accounts and capabilities and derive schema controls', () => {
  const harness = { ...basicHarness, supported_models: { fireworks: ['model-a', 'model-b'] }, default_config: { reasoning_level: 'max', web_search_enabled: false }, config_schema: { properties: { reasoning_level: { enum: ['low', 'max'] }, web_search_enabled: { type: 'boolean', default: true } } } }
  const data = { ...bootstrap, provider_accounts: [
    { id: 'one', name: 'One', provider: 'fireworks', status: 'enabled' },
    { id: 'two', name: 'Two', provider: 'fireworks', status: 'enabled' },
    { id: 'disabled', provider: 'fireworks', status: 'disabled' },
    { id: 'expired', provider: 'fireworks', status: 'reauth_required' },
    ...bootstrap.provider_accounts,
  ] }
  const options = harnessOptions(data, harness)
  assert.deepEqual(options.accounts.map(a => a.account_id), ['one', 'two'])
  assert.deepEqual(options.accounts[0].model_ids, ['model-a', 'model-b'])
  assert.deepEqual(options.reasoningLevels, ['low', 'max'])
  assert.equal(options.defaultReasoning, 'max')
  assert.equal(options.webSearchSupported, true)
  assert.equal(options.defaultWebSearch, false)
  assert.equal(harnessOptions(data, basicHarness).webSearchSupported, false)
  assert.deepEqual(harnessOptions(data, undefined).accounts, [])
})

test('environments harness hides environment picker and has no optional web settings', () => {
  const envHarness = { ...basicHarness, id: 'environments', config_schema: { properties: { reasoning_level: { enum: ['low', 'high'] } } } }
  const html = app(`/projects/${projectId}`, [record], { ...bootstrap, harnesses: [envHarness] })
  assert.ok(html.includes('Harness: environments'))
  assert.ok(!html.includes('Prompt settings'))
  assert.ok(!html.includes('project-environment-reasoning-label--animate'))
  assert.ok(!html.includes('aria-label="Environment:'))
})

test('bootstrap query is scoped, bounded, abortable and retries only transient failures', async () => {
  const options = projectBootstrapOptions(projectId)
  assert.notDeepEqual(options.queryKey, projectBootstrapOptions(otherId).queryKey)
  assert.equal(projectBootstrapOptions('invalid').enabled, false)
  assert.equal(options.retry(0, new ApiError(404, 'missing', null)), false)
  assert.equal(options.retry(0, new ApiError(429, 'limited', null)), true)
  assert.equal(options.retry(1, new ApiError(502, 'gateway', null)), true)
  assert.equal(options.retry(2, new Error('network')), false)
  assert.equal(options.refetchIntervalInBackground, false)
  const original = globalThis.fetch
  const controller = new AbortController()
  globalThis.fetch = async (url, init) => {
    assert.equal(url, `/api/projects/${projectId}/bootstrap`)
    assert.equal(init.signal, controller.signal)
    return { ok: true, json: async () => bootstrap }
  }
  try { assert.equal(await options.queryFn({ signal: controller.signal }), bootstrap) }
  finally { globalThis.fetch = original }
})
test('environments retains the table without an Edit action', () => {
  const html = app(`/projects/${projectId}/environments`, [record])
  assert.match(html, /aria-label="Environments"[^>]*aria-current="page"/)
  assert.match(html, /Back to Dashboard/)
  assert.equal((html.match(/<aside/g) ?? []).length, 1)
  assert.ok(!html.includes('/environments/edit'))
  assert.ok(!html.includes('project-environment-composer'))
  for (const value of ['Example project', 'Local workspace', 'project-environments-table', '/Users/test/workspace', 'src/app']) {
    assert.ok(html.includes(value), value)
  }
  assert.equal((html.match(/scope="col"/g) ?? []).length, 5)
  assert.ok(!app(`/projects/${otherId}/environments`, [record]).includes('Local workspace'))
})
test('invalid project route is handled without showing another project', () => {
  assert.match(app('/projects/not-a-uuid'), /Invalid project ID/)
})
test('table renders machine and sandbox record fields safely', () => {
  const html = table({ data: [record, { ...record, id: otherId, name: '<script>bad</script>', type: 'sandbox', machine_id: null, snapshot_id: record.id, path: '.', updated_at: 'invalid' }] })
  for (const value of ['Machine', 'Sandbox', 'workspace', 'src/app', '&lt;script&gt;bad&lt;/script&gt;']) assert.ok(html.includes(value), value)
  assert.ok(!html.includes('Machine / Snapshot ID'))
  assert.ok(!html.includes(record.id))
  assert.ok(!html.includes(otherId))
  assert.ok(!html.includes('<script>bad</script>'))
  assert.match(html, /<caption/)
  assert.equal((html.match(/scope="col"/g) ?? []).length, 5)
  assert.ok(!html.includes('Updated at'))
  assert.ok(!html.includes('Created at'))
  assert.ok(!html.includes('<time'))
  assert.match(html, /aria-label="Actions for Local workspace" aria-haspopup="menu" aria-expanded="false"/)
  assert.ok(!html.includes('Environment actions are not available yet'))
  assert.match(html, /provider-actions-trigger/)
})
test('table handles empty, loading, offline, and retry states', () => {
  assert.match(table(), /No environments present/)
  assert.match(table({ isPending: true, data: undefined, fetchStatus: 'fetching' }), /Loading environments/)
  assert.match(table({ isPending: true, data: undefined, fetchStatus: 'paused' }), /offline/)
  assert.match(table({ isError: true, data: undefined, error: new Error('Test failure') }), /Test failure.*Retry/)
})
test('background errors retain cached rows', () => {
  const html = table({ isError: true, data: [record], error: new Error('Test failure') })
  assert.match(html, /Local workspace/)
  assert.ok(!html.includes('Couldn’t load environments'))
})
test('name and workspace root omit their IDs while preserving the native path', () => {
  const html = table({ data: [record] })
  assert.match(html, /\/Users\/test\/workspace/)
  assert.match(html, /Local workspace/)
  assert.ok(!html.includes(record.id))
  assert.ok(!html.includes('ID: workspace'))
  assert.ok(!html.includes('environment-record-id'))
  assert.ok(!html.includes(record.machine_id))
})
