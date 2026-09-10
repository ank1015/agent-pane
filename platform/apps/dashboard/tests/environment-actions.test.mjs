import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MutationObserver, QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createTestViteServer } from './vite-test-server.mjs'

const vite = await createTestViteServer()
after(() => vite.close())
const { EnvironmentActionDialog } = await vite.ssrLoadModule('/src/features/projects/EnvironmentActionDialog.tsx')
const { environmentActionOptions, projectKeys } = await vite.ssrLoadModule('/src/features/projects/project-queries.ts')
const projectId = '01900000-0000-7000-8000-000000000001'
const environment = { id: '01900000-0000-7000-8000-000000000002', project_id: projectId, name: 'Desktop test', type: 'machine', workspace_root: '/Users/test/Desktop', path: 'test' }
const other = { ...environment, id: 'other', name: 'Other environment' }

function render(action) {
  const client = new QueryClient()
  try {
    return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(EnvironmentActionDialog, { environment, action, onClose() {} })))
  } finally { client.clear() }
}

test('environment rename uses existing provider field/button styles and the saved name', () => {
  const html = render('rename')
  for (const value of ['Update name', 'Environment name', 'cursor-input', 'update-provider-name-field', 'value="Desktop test"', 'Cancel']) assert.ok(html.includes(value), value)
  assert.match(html, /disabled="">Confirm/)
})

test('delete is an alert confirmation with a brief record-only warning', () => {
  const html = render('delete')
  assert.match(html, /role="alertdialog"/)
  assert.match(html, /Delete Desktop test/)
  assert.match(html, /This environment configuration will be removed\. This action cannot be undone\./)
  assert.match(html, /delete-provider-confirm-button/)
  assert.match(html, /Cancel/)
})

test('rename and delete send project-scoped requests and reconcile table and picker caches', async () => {
  const client = new QueryClient()
  const listKey = projectKeys.environments(projectId)
  const bootstrapKey = projectKeys.bootstrap(projectId)
  const bootstrap = { harnesses: [{ id: 'environments' }], provider_accounts: [{ id: 'account' }], project_environments: [environment, other] }
  client.setQueryData(listKey, [environment, other])
  client.setQueryData(bootstrapKey, bootstrap)
  client.setQueryData(projectKeys.environments('another-project'), [other])
  const options = environmentActionOptions(client, projectId)
  assert.equal(options.retry, false)
  const observer = new MutationObserver(client, options)
  const original = globalThis.fetch
  const renamed = { ...environment, name: 'Zebra workspace' }
  const calls = []
  globalThis.fetch = async (url, init) => {
    calls.push({ url, init })
    assert.equal(url, `/api/projects/${projectId}/environments/${environment.id}`)
    return init.method === 'PATCH' ? new Response(JSON.stringify(renamed)) : new Response(null, { status: 204 })
  }
  try {
    await observer.mutate({ environmentId: environment.id, action: 'rename', name: renamed.name })
    assert.deepEqual(JSON.parse(calls[0].init.body), { name: renamed.name })
    assert.equal(calls[0].init.method, 'PATCH')
    assert.deepEqual(client.getQueryData(listKey), [other, renamed])
    assert.deepEqual(client.getQueryData(bootstrapKey), { ...bootstrap, project_environments: [other, renamed] })
    await observer.mutate({ environmentId: environment.id, action: 'delete' })
    assert.equal(calls[1].init.method, 'DELETE')
    assert.equal(calls[1].init.body, undefined)
    assert.deepEqual(client.getQueryData(listKey), [other])
    assert.deepEqual(client.getQueryData(bootstrapKey).project_environments, [other])
    assert.deepEqual(client.getQueryData(projectKeys.environments('another-project')), [other])
    assert.equal(client.getQueryState(bootstrapKey).isInvalidated, true)
  } finally { globalThis.fetch = original; client.clear() }
})

test('mutation failures preserve records, expose the server error, and do not auto-retry', async () => {
  const original = globalThis.fetch
  try {
    for (const action of ['rename', 'delete']) {
      const client = new QueryClient()
      client.setQueryData(projectKeys.environments(projectId), [environment])
      const observer = new MutationObserver(client, environmentActionOptions(client, projectId))
      let calls = 0
      globalThis.fetch = async () => { calls++; return new Response(JSON.stringify({ error: { message: 'Please try again.' } }), { status: 503 }) }
      try {
        await assert.rejects(observer.mutate({ environmentId: environment.id, action, name: 'New name' }), /Please try again/)
        assert.equal(calls, 1)
        assert.deepEqual(client.getQueryData(projectKeys.environments(projectId)), [environment])
        assert.equal(client.getQueryData(projectKeys.bootstrap(projectId)), undefined)
      } finally { client.clear() }
    }
  } finally { globalThis.fetch = original }
})
