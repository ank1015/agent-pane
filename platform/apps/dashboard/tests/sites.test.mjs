import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createServer as httpServer } from 'node:http'
import { createServer } from 'vite'
import { randomUUID } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import vm from 'node:vm'
const vite = await createServer({ configFile: false, optimizeDeps: { noDiscovery: true, include: [] }, server: { middlewareMode: true, ws: false }, appType: 'custom' })
after(() => vite.close())
const { validSiteCall, backendResult } = await vite.ssrLoadModule('/src/features/sites/site-bridge.ts')
const { sitesBridge } = await vite.ssrLoadModule('/server/sites-bridge.ts')
test('iframe requests cannot select identity, release, URL, or excessive input', () => {
  const call = { id: randomUUID(), endpoint: '/catalog', input: {} }
  assert.equal(validSiteCall(call), true)
  for (const extra of [{ release: randomUUID() }, { project: randomUUID() }, { endpoint: 'https://evil.test' }, { endpoint: '//evil.test' }, { endpoint: '/a?b=1' }, { input: 'x'.repeat(129 * 1024) }]) assert.equal(validSiteCall({ ...call, ...extra }), false)
  assert.throws(() => backendResult({ status: 'failed' }))
  assert.throws(() => backendResult({ status: 'succeeded', response: { status: 503, body: { error: 'Try later' } } }), /Try later/)
  assert.deepEqual(backendResult({ status: 'succeeded', response: { status: 200, body: { ok: true } } }), { ok: true })
})
test('local BFF authenticates upstream, rejects cross-origin callers, and pins release', async () => {
  const project = randomUUID(), site = randomUUID(), release = randomUUID(), token = 'server-only-' + 'x'.repeat(32)
  const seen = []; let activeRelease = release
  const upstream = httpServer(async (req, res) => {
    assert.equal(req.headers.authorization, `Bearer ${token}`)
    let raw = ''; for await (const part of req) raw += part
    seen.push({ path: req.url, method: req.method, body: raw ? JSON.parse(raw) : undefined })
    if (req.method === 'DELETE') { res.writeHead(204); return res.end() }
    res.setHeader('content-type', 'application/json')
    if (req.url.endsWith('/content-access')) return res.end(JSON.stringify({ url: `http://127.0.0.1:3103/content/ticket/${site}/${activeRelease}/index.html`, expires_at: Math.floor(Date.now()/1000)+3600 }))
    if (req.url.endsWith('/invocations')) return res.end(JSON.stringify({ status: 'succeeded', response: { status: 200, body: {} } }))
    res.end(JSON.stringify({ site: { name: 'Demo', desired_status: 'ready' }, resource: { active_release_id: activeRelease } }))
  })
  await new Promise(r => upstream.listen(0, '127.0.0.1', r))
  const server = httpServer(); await new Promise(r => server.listen(0, '127.0.0.1', r))
  const origin = `http://127.0.0.1:${server.address().port}`
  let middleware
  sitesBridge({ DASHBOARD_ORIGIN: origin, DASHBOARD_PLATFORM_URL: `http://127.0.0.1:${upstream.address().port}`, DASHBOARD_SITE_PROJECT_TOKENS: JSON.stringify({ [project]: token }) }).configureServer({ middlewares: { use(fn) { middleware = fn } } })
  server.on('request', (req, res) => middleware(req, res, () => { res.statusCode = 404; res.end() }))
  const path = `/api/site-view/projects/${project}/sites/${site}/open`
  const headers = { 'sec-fetch-site': 'same-origin', origin, 'content-type': 'application/json' }
  try {
    assert.equal((await fetch(origin+path, { method: 'POST', headers: { ...headers, origin: 'null' } })).status, 403)
    assert.equal((await fetch(origin+path, { method: 'POST', headers: { ...headers, 'sec-fetch-site': 'cross-site' } })).status, 403)
    const response = await fetch(origin+path, { method: 'POST', headers }); const view = await response.json()
    assert.equal(response.status, 200); assert.match(response.headers.get('content-security-policy'), /frame-src http:\/\/127\.0\.0\.1:3103/); assert.ok(!JSON.stringify(view).includes(token)); assert.equal(view.releaseId, release)
    activeRelease = randomUUID()
    const invoke = body => fetch(origin+'/api/site-view/invoke', { method: 'POST', headers: { ...headers, 'x-site-view': view.token }, body: JSON.stringify(body) })
    assert.equal((await invoke({ id: randomUUID(), endpoint: '/start', input: {}, releaseId: activeRelease })).status, 400)
    assert.equal((await invoke({ id: randomUUID(), endpoint: '/catalog', input: {} })).status, 200)
    assert.equal(seen.at(-1).body.release_id, release)
    assert.match(seen.at(-1).path, new RegExp(`/projects/${project}/sites/${site}/invocations$`))
    assert.equal((await fetch(origin+'/api/site-view/invoke', { method: 'POST', headers, body: '{}' })).status, 401)
    assert.equal((await fetch(origin+path.replace(project, randomUUID()), { method: 'POST', headers })).status, 503)
    const sitePath = path.replace(/\/open$/, '')
    const count = seen.length
    assert.equal((await fetch(origin+sitePath, { method: 'DELETE', headers: { ...headers, origin: 'null' } })).status, 403)
    assert.equal((await fetch(origin+sitePath+'?site=other', { method: 'DELETE', headers })).status, 400)
    assert.equal((await fetch(origin+sitePath.replace(project, randomUUID()), { method: 'DELETE', headers })).status, 503)
    assert.equal(seen.length, count)
    for (let i = 0; i < 2; i++) {
      const deleted = await fetch(origin+sitePath, { method: 'DELETE', headers })
      assert.equal(deleted.status, 204); assert.equal(await deleted.text(), '')
      assert.deepEqual(seen.at(-1), { path: `/api/projects/${project}/sites/${site}`, method: 'DELETE', body: undefined })
    }
    assert.equal((await invoke({ id: randomUUID(), endpoint: '/catalog', input: {} })).status, 401)

  } finally { server.closeAllConnections(); upstream.closeAllConnections(); await Promise.all([new Promise(r => server.close(r)), new Promise(r => upstream.close(r))]) }
})
test('injected SDK accepts only its configured parent and exposes only callBackend', async () => {
  const source = (await readFile('../sites-service/src/frontend_sdk.js', 'utf8')).replace('__DASHBOARD_ORIGIN__', JSON.stringify('http://127.0.0.1:5173'))
  let listener; const parent = { postMessage() {} }; let sent
  const context = { parent, crypto: { randomUUID }, TextEncoder, setTimeout, clearTimeout, setInterval: () => 1, clearInterval() {}, window: { addEventListener(_name, fn) { listener = fn } } }
  vm.runInNewContext(source, context)
  const port = { start() {}, postMessage(data) { sent = data; queueMicrotask(() => this.onmessage({ data: { id: data.id, result: { harnesses: [] } } })) } }
  listener({ source: {}, origin: 'http://127.0.0.1:5173', data: { type: 'sites:connect:v1' }, ports: [port] })
  assert.equal(port.onmessage, undefined)
  listener({ source: parent, origin: 'https://evil.test', data: { type: 'sites:connect:v1' }, ports: [port] })
  assert.equal(port.onmessage, undefined)
  listener({ source: parent, origin: 'http://127.0.0.1:5173', data: { type: 'sites:connect:v1' }, ports: [port] })
  assert.deepEqual(await context.window.callBackend('/catalog', {}), { harnesses: [] })
  assert.equal(sent.endpoint, '/catalog'); assert.equal(context.window.platform, undefined)
  await assert.rejects(context.window.callBackend('https://evil.test', {}))
})

test('saved snapshot attempts preserve operation identity and discard scope overrides', async () => {
  const { readSiteAttempt } = await vite.ssrLoadModule('/src/features/sites/site-operations.ts')
  const id = randomUUID(), snapshot = randomUUID()
  assert.deepEqual(readSiteAttempt(JSON.stringify({ id, kind: 'restore', snapshot, site: randomUUID() })), { id, kind: 'restore', snapshot })
  assert.deepEqual(readSiteAttempt(JSON.stringify({ id, kind: 'snapshot', name: 'Keep this' })), { id, kind: 'snapshot', name: 'Keep this' })
  for (const value of [null, '{', '{}', JSON.stringify({ id, kind: 'restore', snapshot: '../other' }), JSON.stringify({ id, kind: 'snapshot', name: ' ' })]) assert.equal(readSiteAttempt(value), null)
})

test('snapshot and chat routes stay project/site scoped and retain retry IDs', async () => {
  const project = randomUUID(), site = randomUUID(), snapshot = randomUUID(), operation = randomUUID(), token = 'local-' + 'x'.repeat(32), seen = []
  const upstream = httpServer(async (req, res) => {
    assert.equal(req.headers.authorization, `Bearer ${token}`)
    let raw = ''; for await (const part of req) raw += part
    seen.push({ path: req.url, method: req.method, body: raw ? JSON.parse(raw) : null })
    res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ id: operation, status: 'succeeded', items: [] }))
  })
  const server = httpServer()
  await Promise.all([new Promise(r => upstream.listen(0, '127.0.0.1', r)), new Promise(r => server.listen(0, '127.0.0.1', r))])
  const origin = `http://127.0.0.1:${server.address().port}`; let middleware
  sitesBridge({ DASHBOARD_ORIGIN: origin, DASHBOARD_PLATFORM_URL: `http://127.0.0.1:${upstream.address().port}`, DASHBOARD_SITE_PROJECT_TOKENS: JSON.stringify({ [project]: token }) }).configureServer({ middlewares: { use(fn) { middleware = fn } } })
  server.on('request', (req, res) => middleware(req, res, () => { res.statusCode = 404; res.end() }))
  const base = `/api/site-view/projects/${project}/sites/${site}`, headers = { 'sec-fetch-site': 'same-origin', origin, 'content-type': 'application/json' }
  const send = (path, body, method = body ? 'POST' : 'GET') => fetch(origin + base + path, { method, headers, body: body ? JSON.stringify(body) : undefined })
  try {
    for (const path of ['', '/sessions', '/diagnostics', `/invocations/${operation}`, `/authoring/${operation}`, `/snapshots?limit=20&after=${snapshot}`]) assert.equal((await send(path)).status, 200)
    assert.equal(seen.at(-1).path, `/api/projects/${project}/sites/${site}/snapshots?limit=20&after=${snapshot}`)
    for (let i = 0; i < 2; i++) assert.equal((await send('/snapshots', { id: operation, name: 'Saved' })).status, 200)
    assert.deepEqual(seen.at(-1), seen.at(-2), 'uncertain retries retain the exact body and route')
    assert.equal((await send(`/snapshots/${snapshot}/restore`, { id: operation })).status, 200)
    assert.equal(seen.at(-1).body.id, operation)
    assert.equal((await send('', { name: 'Renamed' }, 'PATCH')).status, 200)
    const count = seen.length
    for (const path of ['/snapshots?limit=51', '/snapshots?limit=2&limit=3', '/snapshots?after=invalid', '/snapshots?project=other', '/sessions?after=1']) assert.equal((await send(path)).status, 400)
    assert.equal((await send('/snapshots', { id: operation, name: 'Saved', siteId: randomUUID() })).status, 400)
    assert.equal((await send(`/snapshots/${snapshot}/restore`, { id: operation, source: 'injected' })).status, 400)
    assert.equal((await send('', { name: 'Renamed', desired_status: 'deleted' }, 'PATCH')).status, 400)
    assert.equal((await send('', { name: 'x'.repeat(129) }, 'PATCH')).status, 400)
    assert.equal(seen.length, count)
  } finally { server.closeAllConnections(); upstream.closeAllConnections(); await Promise.all([new Promise(r => server.close(r)), new Promise(r => upstream.close(r))]) }
})

test('Sites keeps the environments table styling and Open links use new tabs', async () => {
  const { createElement } = await import('react')
  const { renderToStaticMarkup } = await import('react-dom/server')
  const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query')
  const { MemoryRouter, Routes, Route } = await import('react-router-dom')
  const { ProjectSitesPage } = await vite.ssrLoadModule('/src/features/sites/ProjectSitesPage.tsx')
  const { siteKeys } = await vite.ssrLoadModule('/src/features/sites/site-queries.ts')
  const project = randomUUID(), site = randomUUID(), client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity } } })
  client.setQueryData(siteKeys.list(project), { items: [{ id: site, name: '<Demo>', desired_status: 'ready', updated_at: '2026-09-08T00:00:00Z' }] })
  try {
    const html = renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(MemoryRouter, { initialEntries: [`/projects/${project}/sites`] }, createElement(Routes, null, createElement(Route, { path: '/projects/:projectId/sites', element: createElement(ProjectSitesPage) })))))
    assert.match(html, /class="providers-table-wrap"/); assert.match(html, /<table class="providers-table">/)
    assert.match(html, /&lt;Demo&gt;/); assert.doesNotMatch(html, /<iframe/)
    const open = html.match(/<a[^>]*aria-label="Open &lt;Demo&gt; in a new tab"[^>]*>/)?.[0]
    assert.ok(open); assert.ok(open.includes(`href="/projects/${project}/sites/${site}/view"`))
    assert.match(open, /target="_blank"/); assert.match(open, /rel="noopener noreferrer"/)
    assert.doesNotMatch(html, /Edit with Sites/)
    assert.match(html, /aria-label="Actions for &lt;Demo&gt;"/); assert.match(html, /aria-haspopup="menu"/)
  } finally { client.clear() }
})
