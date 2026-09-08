import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MutationObserver, QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createServer } from 'vite'

const vite = await createServer({ server: { middlewareMode: true, ws: false }, appType: 'custom' })
after(() => vite.close())
const { AddHarnessesDialog } = await vite.ssrLoadModule('/src/features/projects/AddHarnessesDialog.tsx')
const { ProjectHarnessesTable } = await vite.ssrLoadModule('/src/features/projects/ProjectHarnessesTable.tsx')
const { harnessUpdateOptions, projectHarnessesOptions } = await vite.ssrLoadModule('/src/features/projects/harness-queries.ts')
const { projectKeys } = await vite.ssrLoadModule('/src/features/projects/project-queries.ts')
const { ApiError } = await vite.ssrLoadModule('/src/lib/api-client.ts')
const project = '01900000-0000-7000-8000-000000000001'
const harness = (id, project_enabled, project_policy = 'opt_in') => ({ id, name: id, description: null, project_enabled, project_policy, enabled: true, globally_enabled: true, available: project_enabled, supported_models: {}, config_schema: null, default_config: {} })
const required = harness('Environments', true, 'required')
const sites = harness('Sites', true, 'required')
const optional = harness('Basic CC', true)
const unused = harness('Unused', false)

test('table shows platform-owned Sites without a remove button', () => {
  const client = new QueryClient()
  client.setQueryData(projectKeys.harnesses(project), { items: [required, sites, optional, unused] })
  try {
    const html = renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(ProjectHarnessesTable, { projectId: project })))
    for (const value of ['providers-table', 'provider-add-button', 'Environments', 'Sites', 'Platform', 'Basic CC', 'Opt-in', 'Remove Basic CC']) assert.ok(html.includes(value), value)
    assert.ok(!html.includes('Remove Environments'))
    assert.ok(!html.includes('Remove Sites'))
    assert.ok(!html.includes('Unused'))
  } finally { client.clear() }
})

test('add dialog shows only unselected opt-ins as accessible switches and requires a selection', () => {
  const html = renderToStaticMarkup(createElement(AddHarnessesDialog, { harnesses: [required, optional, { ...unused, description: 'Hidden description', globally_enabled: false }, harness('Second', false)], busy: false, onConfirm() {}, onClose() {} }))
  assert.match(html, /Add harnesses/)
  assert.equal((html.match(/role="switch"/g) ?? []).length, 2)
  assert.match(html, /aria-checked="false"/)
  assert.match(html, /disabled="">Confirm/)
  assert.ok(!html.includes('Basic CC'))
  assert.ok(!html.includes('Environments'))
  assert.ok(!html.includes('Hidden description'))
  assert.ok(!html.includes('Currently unavailable on the platform'))
  const empty = renderToStaticMarkup(createElement(AddHarnessesDialog, { harnesses: [required, optional], busy: false, onConfirm() {}, onClose() {} }))
  assert.match(empty, /All harnesses have been added/)
})

test('queries use project-scoped cancellation and bounded retries only for transient errors', async () => {
  const options = projectHarnessesOptions(project)
  assert.deepEqual(options.queryKey, projectKeys.harnesses(project))
  assert.equal(options.retry(0, new ApiError(404, 'missing', {})), false)
  assert.equal(options.retry(0, new ApiError(503, 'busy', {})), true)
  assert.equal(options.retry(2, new Error('offline')), false)
  assert.equal(options.refetchIntervalInBackground, false)
  const original = globalThis.fetch
  const signal = new AbortController().signal
  globalThis.fetch = async (url, init) => {
    assert.equal(url, `/api/projects/${project}/harnesses`)
    assert.equal(init.signal, signal)
    return new Response(JSON.stringify({ items: [required] }))
  }
  try { assert.deepEqual(await options.queryFn({ signal }), { items: [required] }) }
  finally { globalThis.fetch = original }
})

test('batch updates retain partial success, report failures, and update only this project’s picker and inventory', async () => {
  const client = new QueryClient()
  const catalogKey = projectKeys.harnesses(project)
  const bootstrapKey = projectKeys.bootstrap(project)
  const bootstrap = { harnesses: [required, optional], provider_accounts: [{ id: 'account' }], project_environments: [{ id: 'environment' }] }
  client.setQueryData(catalogKey, { items: [required, optional, unused, harness('Failed', false)] })
  client.setQueryData(bootstrapKey, bootstrap)
  client.setQueryData(projectKeys.harnesses('other'), { items: [unused] })
  const original = globalThis.fetch
  const calls = []
  globalThis.fetch = async (url, init) => {
    calls.push(url)
    assert.equal(init.method, 'PUT')
    assert.ok(init.signal)
    assert.deepEqual(JSON.parse(init.body), { enabled: true })
    return url.endsWith('/Failed')
      ? new Response(JSON.stringify({ error: { message: 'Please try again.' } }), { status: 503 })
      : new Response(JSON.stringify({ ...unused, available: true, project_enabled: true }))
  }
  try {
    const options = harnessUpdateOptions(client, project)
    assert.equal(options.retry, false)
    const result = await new MutationObserver(client, options).mutate({ ids: ['Unused', 'Failed', 'Unused'], enabled: true })
    assert.equal(calls.length, 2)
    assert.equal(result.updated.length, 1)
    assert.deepEqual(result.failed, [{ id: 'Failed', message: 'Please try again.' }])
    assert.equal(client.getQueryData(catalogKey).items.find(h => h.id === 'Unused').project_enabled, true)
    assert.equal(client.getQueryData(catalogKey).items.find(h => h.id === 'Failed').project_enabled, false)
    assert.equal(client.getQueryData(bootstrapKey).harnesses.length, 3)
    assert.deepEqual(client.getQueryData(bootstrapKey).provider_accounts, bootstrap.provider_accounts)
    assert.deepEqual(client.getQueryData(bootstrapKey).project_environments, bootstrap.project_environments)
    assert.deepEqual(client.getQueryData(projectKeys.harnesses('other')), { items: [unused] })
    assert.equal(client.getQueryState(bootstrapKey).isInvalidated, true)
  } finally { globalThis.fetch = original; client.clear() }
})

test('removal conflicts preserve the picker; successful disable removes it immediately', async () => {
  const client = new QueryClient()
  const key = projectKeys.bootstrap(project)
  client.setQueryData(key, { harnesses: [required, optional], provider_accounts: [], project_environments: [] })
  const original = globalThis.fetch
  globalThis.fetch = async () => new Response(JSON.stringify({ error: { message: 'Finish or abort active runs first.' } }), { status: 409 })
  try {
    const observer = new MutationObserver(client, harnessUpdateOptions(client, project))
    const result = await observer.mutate({ ids: [optional.id], enabled: false })
    assert.match(result.failed[0].message, /active runs/)
    assert.equal(client.getQueryData(key).harnesses.length, 2)
    globalThis.fetch = async (_url, init) => {
      assert.deepEqual(JSON.parse(init.body), { enabled: false })
      return new Response(JSON.stringify({ ...optional, available: false, project_enabled: false }))
    }
    await observer.mutate({ ids: [optional.id], enabled: false })
    assert.deepEqual(client.getQueryData(key).harnesses, [required])
    assert.equal(client.getQueryData(projectKeys.harnesses(project)), undefined)
  } finally { globalThis.fetch = original; client.clear() }
})
