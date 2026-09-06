/* eslint-disable no-control-regex -- Reject control characters in untrusted endpoint paths. */
// Trusted local dashboard BFF. These variables are never exposed through VITE_*.
import { randomBytes } from 'node:crypto'
import type { IncomingMessage, ServerResponse } from 'node:http'
import type { Plugin } from 'vite'
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const prefix = '/api/site-view'
type View = { project: string; site: string; release: string; expires: number }
export function sitesBridge(env: Record<string, string>): Plugin {
  const origin = env.DASHBOARD_ORIGIN || 'http://127.0.0.1:5173'
  const contentOrigin = env.DASHBOARD_SITES_CONTENT_ORIGIN || 'http://127.0.0.1:3103'
  const upstream = env.DASHBOARD_PLATFORM_URL || 'http://127.0.0.1:3100'
  for (const value of [origin, contentOrigin, upstream]) {
    const u = new URL(value)
    if (u.origin !== value || u.username || u.password || !(u.protocol === 'https:' || u.protocol === 'http:' && ['127.0.0.1', 'localhost', '[::1]'].includes(u.hostname))) throw new Error('Sites bridge requires HTTPS origins or local loopback HTTP origins')
  }
  if (origin === contentOrigin) throw new Error('Site content must use a separate origin')
  const tokens = JSON.parse(env.DASHBOARD_SITE_PROJECT_TOKENS || '{}') as Record<string, string>
  if (!tokens || Array.isArray(tokens) || typeof tokens !== 'object' || Object.entries(tokens).some(([key, value]) => !uuid.test(key) || typeof value !== 'string' || !/^[!-~]{32,256}$/.test(value))) throw new Error('Invalid server-only project token map')
  const views = new Map<string, View>()
  let active = 0
  const fail = (status: number, message: string) => Object.assign(new Error(message), { status })
  async function platform(project: string, path: string, method = 'GET', body?: unknown) {
    if (!tokens[project]) throw fail(503, 'Sites access is not configured for this project.')
    const response = await fetch(`${upstream}/api/projects/${project}/sites${path}`, { method, redirect: 'error', signal: AbortSignal.timeout(40_000), headers: { Authorization: `Bearer ${tokens[project]}`, 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) })
    const reader = response.body?.getReader(); const chunks: Uint8Array[] = []; let length = 0
    if (reader) for (;;) { const chunk = await reader.read(); if (chunk.done) break; length += chunk.value.length; if (length > 512 * 1024) { await reader.cancel(); throw fail(502, 'Site response is too large.') }; chunks.push(chunk.value) }
    const value = JSON.parse(Buffer.concat(chunks).toString())
    if (!response.ok) throw fail(response.status, value?.error?.message || 'Site request failed.')
    return value
  }
  async function read(req: IncomingMessage) {
    if (!req.headers['content-type']?.startsWith('application/json')) throw fail(415, 'Expected JSON.')
    let text = ''
    for await (const chunk of req) { text += chunk; if (Buffer.byteLength(text) > 128 * 1024) throw fail(413, 'Site request is too large.') }
    try { return JSON.parse(text) } catch { throw fail(400, 'Invalid JSON.') }
  }
  async function middleware(req: IncomingMessage, res: ServerResponse, next: () => void) {
    // Also constrains navigation of the iframe itself, including script-initiated
    // navigation. Content responses separately restrict scripts/assets/network.
    res.setHeader('Content-Security-Policy', `frame-src ${contentOrigin}; object-src 'none'`)
    if (!req.url?.startsWith(prefix)) return next()
    const json = (status: number, value: unknown) => { res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' }); res.end(JSON.stringify(value)) }
    if (req.headers.host !== new URL(origin).host || req.headers['sec-fetch-site'] !== 'same-origin' || req.headers.origin && req.headers.origin !== origin) return json(403, { error: { message: 'Site requests require the configured dashboard origin.' } })
    if (active >= 16) return json(429, { error: { message: 'Too many site requests. Try again shortly.' } })
    active++
    try {
      const url = new URL(req.url, origin)
      if (url.search) throw fail(400, 'Unexpected query parameters.')
      const parts = url.pathname.slice(prefix.length).split('/').filter(Boolean)
      for (const [key, view] of views) if (view.expires <= Date.now()) views.delete(key)
      if (parts[0] === 'projects' && uuid.test(parts[1] || '') && parts[2] === 'sites') {
        const project = parts[1].toLowerCase()
        if (req.method === 'GET' && parts.length === 3) return json(200, await platform(project, ''))
        if (req.method === 'POST' && parts.length === 5 && uuid.test(parts[3]) && parts[4] === 'open') {
          const site = parts[3].toLowerCase()
          const data = await platform(project, `/${site}`)
          if (data.site.desired_status !== 'ready') throw fail(409, 'This site is currently unavailable.')
          const release = data.resource.active_release_id
          if (!uuid.test(release || '')) throw fail(409, 'This site has no published active release yet.')
          const access = await platform(project, `/${site}/content-access`, 'POST', { release_id: release, ttl_seconds: 3600 })
          const asset = new URL(access.url)
          if (asset.origin !== contentOrigin || !asset.pathname.includes(`/${site}/${release}/`) || asset.username || asset.password || asset.search || asset.hash) throw fail(502, 'Unexpected site content address.')
          if (views.size >= 512) throw fail(429, 'Too many open site views. Try again later.')
          const token = randomBytes(32).toString('hex')
          const expires = Math.min(Number(access.expires_at) * 1000, Date.now() + 3600_000)
          if (!Number.isFinite(expires) || expires <= Date.now()) throw fail(502, 'Invalid site content expiry.')
          views.set(token, { project, site, release, expires })
          return json(200, { name: data.site.name, url: access.url, releaseId: release, token, expiresAt: expires })
        }
      }
      if (req.method === 'POST' && parts.length === 1 && parts[0] === 'invoke') {
        const view = views.get(String(req.headers['x-site-view'] || ''))
        if (!view) throw fail(401, 'This site view expired. Reload the page to reconnect.')
        const input = await read(req)
        if (!input || !uuid.test(input.id || '') || typeof input.endpoint !== 'string' || !input.endpoint.startsWith('/') || input.endpoint.startsWith('//') || input.endpoint.length > 2048 || /[?#\x00-\x1f\x7f]/.test(input.endpoint) || Object.keys(input).some(k => !['id', 'endpoint', 'input'].includes(k))) throw fail(400, 'Invalid backend call.')
        return json(200, await platform(view.project, `/${view.site}/invocations`, 'POST', { id: input.id, release_id: view.release, timeout_ms: 30000, request: { method: 'POST', path: input.endpoint, body: input.input ?? null } }))
      }
      throw fail(404, 'Site route not found.')
    } catch (error) {
      const e = error as Error & { status?: number }
      json(e.status || 502, { error: { message: e.status ? e.message : 'The site service could not be reached. Retry with the same application operation key.' } })
    } finally { active-- }
  }
  return { name: 'trusted-sites-bridge', configureServer(server) { server.middlewares.use(middleware) }, configurePreviewServer(server) { server.middlewares.use(middleware) } }
}
