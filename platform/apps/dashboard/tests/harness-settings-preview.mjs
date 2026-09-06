// Isolated browser fixture; never forwards requests to a real platform/gateway.
// Run from dashboard: node tests/harness-settings-preview.mjs
import { createServer } from 'vite'
const project = '01900000-0000-7000-8000-000000000001'
const make = (id, name, enabled, policy = 'opt_in') => ({ id, name, description: 'A harness for everyday coding and experiments.', project_policy: policy, project_enabled: enabled, available: enabled, globally_enabled: true, enabled: true, default_config: {}, config_schema: null, supported_models: {} })
const items = [make('environments', 'Environments', true, 'required'), make('busy', 'Busy harness', true), make('basic', 'Basic CC Tools Harness', false), make('experimental', 'Experimental harness', false), make('retry', 'Retry harness', false)]
let failOnce = true
const server = await createServer({ server: { host: '127.0.0.1', port: 5187, strictPort: true }, plugins: [{
  name: 'isolated-harness-settings',
  configureServer(server) {
    server.middlewares.use(async (req, res, next) => {
      if (!req.url?.startsWith('/api/')) return next()
      const url = new URL(req.url, 'http://fixture')
      const send = (body, status = 200) => { res.statusCode = status; res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify(body)) }
      const path = `/api/projects/${project}`
      if (req.method === 'PUT' && url.pathname.startsWith(`${path}/harnesses/`)) {
        let raw = ''; for await (const chunk of req) raw += chunk
        const { enabled } = JSON.parse(raw)
        const h = items.find(h => h.id === url.pathname.split('/').at(-1))
        if (!h) return send({}, 404)
        if (h.id === 'busy' && !enabled) return send({ error: { message: 'Finish or abort active runs before disabling this harness.' } }, 409)
        if (h.id === 'retry' && failOnce) { failOnce = false; return send({ error: { message: 'Temporarily unavailable. Try again.' } }, 503) }
        h.project_enabled = enabled; h.available = enabled
        return send(h)
      }
      if (url.pathname === `${path}/harnesses`) return send({ items })
      if (url.pathname === `${path}/bootstrap`) return send({ harnesses: items.filter(h => h.available), provider_accounts: [], project_environments: [] })
      if (url.pathname === '/api/projects') return send([{ id: project, name: 'Harness settings preview', avatar: null }])
      if (url.pathname === `${path}/sessions`) return send({ items: [], next_cursor: null })
      return send({ error: { message: 'Unknown fixture route' } }, 404)
    })
  },
}] })
await server.listen()
console.log(`http://127.0.0.1:5187/projects/${project}/settings`)
