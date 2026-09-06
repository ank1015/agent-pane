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
    seen.push({ path: req.url, body: raw ? JSON.parse(raw) : undefined })
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
