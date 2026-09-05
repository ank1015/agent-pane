import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router-dom'
import { createServer } from 'vite'

const vite = await createServer({ server: { middlewareMode: true, ws: false }, appType: 'custom' })
after(() => vite.close())
const { ProjectRecentChats } = await vite.ssrLoadModule('/src/features/projects/ProjectRecentChats.tsx')
const { sessionsOptions, sessionKeys } = await vite.ssrLoadModule('/src/features/projects/session-queries.ts')
const { ApiError } = await vite.ssrLoadModule('/src/lib/api-client.ts')
const projectId = '01900000-0000-7000-8000-000000000001'
const session = { id: 'chat-one', project_id: projectId, title: 'A <chat>', active_run: null }
function render(items, next = null) {
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity } } })
  if (items !== undefined) client.setQueryData(sessionKeys.list(projectId), { pages: [{ items, next_cursor: next }], pageParams: [null] })
  try {
    return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, { initialEntries: [`/projects/${projectId}/chat-one`] }, createElement(ProjectRecentChats, { projectId }))))
  } finally { client.clear() }
}
test('recent chats preserve row styling, selection, escaped titles and hover actions', () => {
  const html = render([session])
  assert.match(html, /Recent Chats/)
  assert.match(html, /project-recents-action.*aria-label="New chat"/)
  assert.match(html, /project-session-item--active/)
  assert.match(html, /aria-current="page"/)
  assert.match(html, /A &lt;chat&gt;/)
  assert.match(html, /project-session-archive/)
  assert.match(html, new RegExp(`href="/projects/${projectId}/chat-one"`))
})
test('loading uses a small spinner without visible loading text; empty results show the empty label', () => {
  const pending = render(undefined)
  assert.match(pending, /aria-label="Loading chats"/)
  assert.match(pending, /project-session-spinner/)
  assert.ok(!pending.includes('Loading sessions'))
  assert.ok(!pending.includes('No Chats yet'))
  const empty = render([])
  assert.match(empty, /No Chats yet/)
  assert.ok(!empty.includes('Loading chats'))
})
test('active runs show activity instead of archive and pagination remains available', () => {
  for (const [status, label] of [['ready', 'queued'], ['running', 'running'], ['waiting', 'waiting']]) {
    const html = render([{ ...session, title: null, active_run: { id: 'run', status } }], 'cursor')
    assert.match(html, new RegExp(`Session is ${label}`))
    assert.match(html, /Untitled chat/)
    assert.match(html, /Load more/)
    assert.ok(!html.includes('project-session-archive'))
  }
})
test('session polling is faster during active runs and retries are bounded', () => {
  const options = sessionsOptions(projectId)
  assert.equal(options.refetchInterval({ state: { data: { pages: [{ items: [session] }] } } }), 30000)
  assert.equal(options.refetchInterval({ state: { data: { pages: [{ items: [{ ...session, active_run: { status: 'waiting' } }] }] } } }), 3000)
  assert.equal(options.refetchIntervalInBackground, false)
  assert.equal(options.retry(0, new ApiError(404, 'missing')), false)
  assert.equal(options.retry(0, new ApiError(503, 'unavailable')), true)
  assert.equal(options.retry(2, new Error('offline')), false)
  assert.equal(options.getNextPageParam({ next_cursor: 'repeat' }, [], null, [null, 'repeat']), undefined)
  assert.notDeepEqual(sessionKeys.list(projectId), sessionKeys.list('another-project'))
  assert.equal(sessionsOptions('invalid').enabled, false)
})
