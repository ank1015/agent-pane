import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router-dom'
import { createServer } from 'vite'

const vite = await createServer({ server: { middlewareMode: true, ws: false }, appType: 'custom' })
after(() => vite.close())
const { ProjectEnvironmentsTable } = await vite.ssrLoadModule('/src/features/projects/ProjectEnvironmentsTable.tsx')
const { default: App } = await vite.ssrLoadModule('/src/App.tsx')
const { projectKeys } = await vite.ssrLoadModule('/src/features/projects/project-queries.ts')
const projectId = '01900000-0000-7000-8000-000000000001'
const otherId = '01900000-0000-7000-8000-000000000002'
const record = { id: '01900000-0000-7000-8000-000000000003', project_id: projectId, name: 'Local workspace', type: 'machine', machine_id: otherId, snapshot_id: null, workspace_root: 'workspace', workspace_root_path: '/Users/test/workspace', path: 'src/app', created_at: '2026-09-04T00:00:00Z', updated_at: '2026-09-04T01:00:00Z' }
function table(overrides = {}) {
  return renderToStaticMarkup(createElement(ProjectEnvironmentsTable, { query: { isPending: false, isError: false, isFetching: false, data: [], refetch: () => {}, ...overrides } }))
}
function app(path, environments = []) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } })
  client.setQueryData(projectKeys.list(), [{ id: projectId, name: 'Example project', avatar: null }])
  client.setQueryData(projectKeys.environments(projectId), environments)
  try {
    return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, { initialEntries: [path] }, createElement(App))))
  } finally { client.clear() }
}

test('project cards use real project detail links', () => {
  assert.match(app('/projects'), new RegExp(`href="/projects/${projectId}"`))
})
test('detail route renders the project name, first tab, back link, and only its environments', () => {
  const html = app(`/projects/${projectId}`, [record])
  for (const value of ['Example project', 'Back to Dashboard', 'aria-current="page"', 'Local workspace', 'href="/projects"']) assert.ok(html.includes(value), value)
  assert.ok(!app(`/projects/${otherId}`, [record]).includes('Local workspace'))
  assert.notDeepEqual(projectKeys.environments(projectId), projectKeys.environments(otherId))
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
  assert.match(html, /aria-label="Actions for Local workspace" disabled=""/)
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
  assert.match(table({ data: [{ ...record, workspace_root_path: null }] }), /Not yet known/)
})
