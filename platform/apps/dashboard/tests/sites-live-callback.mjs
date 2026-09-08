// Opt-in companion for an authored callback test site; see SITES.md.
import assert from 'node:assert/strict'
import { readFile, writeFile } from 'node:fs/promises'
import { chromium } from '../../../packages/harnesses/sites/browser/node_modules/playwright/index.mjs'
if (process.env.SITES_LIVE_E2E !== '1' || !process.env.SITES_E2E_STATE) throw Error('Set SITES_LIVE_E2E=1 and SITES_E2E_STATE to opt into test-site changes and model usage.')
const phase = process.env.SITES_CALLBACK_PHASE || 'verify'
if (!['launch', 'verify'].includes(phase)) throw Error('SITES_CALLBACK_PHASE must be launch or verify')
const statePath = process.env.SITES_E2E_STATE, s = JSON.parse(await readFile(statePath, 'utf8'))
const origin = process.env.DASHBOARD_ORIGIN || 'http://127.0.0.1:5173'
const browser = await chromium.launch()
try {
  const page = await browser.newPage(), errors = []
  page.on('pageerror', e => errors.push(e.message))
  await page.goto(`${origin}/projects/${s.project}/sites/${s.site}/view`)
  const frame = page.frameLocator('iframe')
  await frame.locator('#job-status').waitFor()
  const invoke = (path, input) => frame.locator('body').evaluate(async (_, args) => window.callBackend(args.path, args.input), { path, input })
  let job
  if (phase === 'launch') {
    if (s.callbackJob) throw Error('State already contains a callback job; verify it or use a new test state.')
    await frame.locator('#launch:enabled').waitFor()
    await frame.locator('#launch').click()
    for (let i = 0; i < 30; i++) {
      await page.waitForTimeout(1000)
      job = (await invoke('/latest-job', {})).job
      if (job?.run_id) break
    }
    assert.ok(job?.run_id)
    s.callbackJob = job
    await writeFile(statePath, JSON.stringify(s, null, 2))
  } else {
    assert.ok(s.callbackJob, 'Run the launch phase first')
    await frame.getByText('Callback child completed.', { exact: true }).waitFor({ timeout: 180_000 })
    job = await invoke('/job', { operationKey: s.callbackJob.operation_key })
    assert.equal(job.status, 'completed')
    assert.equal(job.run_id, s.callbackJob.run_id)
    assert.ok(job.event_id)
    assert.equal(await frame.locator('#count').innerText(), String(s.counter))
  }
  const replay = await invoke('/launch', { operationKey: job.operation_key })
  assert.equal(replay.run_id, job.run_id)
  assert.equal(replay.session_id, job.session_id)
  if (phase === 'verify') assert.equal(replay.status, 'completed')
  assert.deepEqual(errors, [])
  s.callbackJob = replay
  s.callbackPassed = phase === 'verify'
  await writeFile(statePath, JSON.stringify(s, null, 2))
  console.log(`PASS: callback ${phase}; duplicate launch kept session ${job.session_id} and run ${job.run_id}`)
} finally { await browser.close() }
