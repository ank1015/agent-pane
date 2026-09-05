// Isolated browser fixture: node tests/chat-preview.mjs (no real gateway calls).
import { createServer } from 'vite'
import { randomUUID } from 'node:crypto'
const project = '01900000-0000-7000-8000-000000000001'
const sessionId = '01900000-0000-7000-8000-000000000002'
const account = '01900000-0000-7000-8000-000000000003'
let session
const runs = [], inputs = [], messages = [], events = [], requests = []
const listeners = new Set()
const receipts = new Map()
let dropResponses = 0
const now = () => new Date().toISOString()
const harness = { id: 'basic-cc-tools-harness', name: 'Basic CC Tools Harness', enabled: true, default_config: { reasoning_level: 'high' }, supported_models: { openai: ['gpt-5.6-sol'] }, config_schema: { properties: { environment: { type: 'object' }, reasoning_level: { enum: ['low', 'medium', 'high'] } } } }
const env = { id: account, project_id: project, name: 'Desktop test', type: 'machine', machine_id: account, workspace_root: '/Users/example/Desktop', path: 'test' }
function event(run, type) {
  run.version++
  const e = { id: randomUUID(), run_id: run.id, sequence: ++run.last_event_sequence, type, payload: {} }
  events.push(e)
  for (const item of listeners) if (item.id === run.id) item.res.write('event: ' + type + '\ndata: ' + JSON.stringify(e) + '\n\n')
}
function append(run, body) {
  const record = { message_id: randomUUID(), revision: ++session.current_revision, run_id: run.id, origin_run_id: run.id, message: body, created_at: now() }
  messages.push(record)
  return record
}
function addInput(run, message) {
  const input = { id: randomUUID(), run_id: run.id, kind: 'user_message', payload: { message }, status: 'pending', handling: null, created_at: now() }
  inputs.push(input)
  event(run, 'run.input_received')
  return input
}
function handleInputs(run) {
  for (const input of inputs.filter(i => i.run_id === run.id && i.status === 'pending')) {
    input.handling = { message_id: append(run, input.payload.message).message_id }
    input.status = 'handled'
  }
}
function start(message) {
  const run = { id: randomUUID(), project_id: project, session_id: sessionId, status: 'ready', config: session.config, version: 0, last_event_sequence: 0, created_at: now(), started_at: null, finished_at: null, abort_requested_at: null, final_message_id: null, error: null }
  runs.push(run); session.active_run = run
  const input = addInput(run, message)
  setTimeout(() => {
    if (run.status === 'aborted') return
    run.status = 'running'; run.started_at = now(); handleInputs(run)
    append(run, { role: 'assistant', id: randomUUID(), content: [{ type: 'thinking', thinking_text: 'I will inspect the workspace, then verify the setup.' }, { type: 'tool_call', tool_call_id: 'call-' + run.id, name: 'read', arguments: { path: 'README.md' } }], timestamp: Date.now(), stop_reason: 'tool_use' })
    event(run, 'run.committed')
  }, 2000)
  setTimeout(() => {
    if (run.status === 'aborted') return
    handleInputs(run)
    append(run, { role: 'tool_result', id: randomUUID(), tool_name: 'read', tool_call_id: 'call-' + run.id, content: [{ type: 'text', content: '# Example workspace\nEverything is configured.' }], outcome: { status: 'success' }, timestamp: Date.now() })
    run.final_message_id = append(run, { role: 'assistant', id: randomUUID(), content: [{ type: 'response', response: { content: '## Environment ready\n\nThe **workspace** is configured.\n\n| Check | Result |\n| --- | --- |\n| Files | Ready |\n| Tests | Passed |\n\nRun this to get started:\n\n```bash\npnpm test\n```\n\n- Setup verified\n- No further changes needed' } }], usage: { input: 100, output: 60, cost: { total: 0.002 } }, timestamp: Date.now(), stop_reason: 'stop' }).message_id
    run.status = 'completed'; run.finished_at = now(); session.active_run = null
    event(run, 'run.completed')
  }, 12000)
  return { run, input }
}
const fixture = {
  name: 'isolated-chat-fixture',
  configureServer(server) {
    server.middlewares.use(async (req, res, next) => {
      const url = new URL(req.url, 'http://fixture')
      if (!url.pathname.startsWith('/api/')) return next()
      const path = url.pathname
      const json = (data, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(data)) }
      if (path === '/api/fixture/requests') return json(requests)
      if (path === '/api/fixture/drop-next') { dropResponses = 3; return json({ ok: true }) }
      if (path === '/api/fixture/disconnect') { for (const { res: stream } of listeners) stream.end(); return json({ ok: true }) }
      if (path === '/api/projects') return json([{ id: project, name: 'Chat verification', avatar: null }])
      if (path.endsWith('/bootstrap')) return json({ harnesses: [harness, { ...harness, id: 'environments', name: 'Environments', config_schema: { properties: { reasoning_level: { enum: ['low', 'high'] } } } }], provider_accounts: [{ id: account, provider: 'openai', name: 'Personal', status: 'enabled' }], project_environments: [env] })
      if (path.endsWith('/environments')) return json([env])
      if (path.endsWith('/events/stream')) {
        const id = path.split('/')[3]
        res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', connection: 'keep-alive' })
        res.write(': connected\n\n')
        for (const e of events.filter(e => e.run_id === id && e.sequence > Number(url.searchParams.get('after_sequence') || 0))) res.write('event: ' + e.type + '\ndata: ' + JSON.stringify(e) + '\n\n')
        const item = { id, res }; listeners.add(item)
        res.on('close', () => listeners.delete(item))
        return
      }
      if (req.method === 'POST') {
        let raw = ''; for await (const chunk of req) raw += chunk
        const body = JSON.parse(raw), key = req.headers['idempotency-key']
        requests.push({ path, body, key })
        if (!key) return json({ error: { message: 'Missing idempotency key' } }, 400)
        const sendReceipt = value => {
          if (dropResponses > 0) { dropResponses--; return json({ error: { message: 'Simulated lost response. Retry the same send.' } }, 503) }
          return json(value, 201)
        }
        if (receipts.has(key)) return sendReceipt(receipts.get(key))
        let reply
        if (path === '/api/projects/' + project + '/sessions') {
          session = { id: sessionId, project_id: project, harness_id: body.harness_id, config: body.config_override, title: body.title, current_revision: 0, active_run: null, archived_at: null }
          reply = { session, ...start(body.initial_run.input) }
        } else if (path.endsWith('/abort')) {
          const run = runs.find(r => r.id === path.split('/')[3]); run.abort_requested_at = now()
          event(run, 'run.abort_requested'); reply = { run }
          setTimeout(() => { run.status = 'aborted'; run.finished_at = now(); session.active_run = null; event(run, 'run.aborted') }, 1000)
        } else if (path.startsWith('/api/runs/') && path.endsWith('/inputs')) {
          const run = runs.find(r => r.id === path.split('/')[3]); reply = { input: addInput(run, body.message), run }
        } else if (path.endsWith('/runs')) {
          if (body.config_override || body.expected_session_revision !== session.current_revision) return json({ error: { message: 'Refresh the session revision.' } }, 409)
          reply = start(body.input)
        }
        if (!reply) return json({ error: { message: 'Unknown mutation' } }, 404)
        const saved = JSON.parse(JSON.stringify(reply)); receipts.set(key, saved); return sendReceipt(saved)
      }
      if (path === '/api/projects/' + project + '/sessions') return json({ items: session ? [session] : [], next_cursor: null })
      if (path === '/api/sessions/' + sessionId && session) return json(session)
      if (path.endsWith('/messages')) return json({ items: messages.filter(m => m.revision > Number(url.searchParams.get('after_revision') || 0)), next_after_revision: null })
      if (path.endsWith('/runs')) return json({ items: runs, next_cursor: null })
      if (path.endsWith('/inputs')) return json({ items: inputs, next_cursor: null })
      return json({ error: { message: 'Not found' } }, 404)
    })
  },
}
const server = await createServer({ plugins: [fixture], server: { host: '127.0.0.1', port: 5174, strictPort: true, proxy: {} } })
await server.listen()
console.log('Chat fixture: http://127.0.0.1:5174/projects/' + project)
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { for (const { res } of listeners) res.end(); await server.close(); process.exit(0) })
