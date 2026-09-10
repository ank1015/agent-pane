// Opt-in real browser/model smoke test. See server/SITES.md for prerequisites.
import assert from 'node:assert/strict'
import { readFile, writeFile } from 'node:fs/promises'
import { chromium } from './browser/node_modules/playwright/index.mjs'
if (process.env.SITES_LIVE_E2E !== '1' || !process.env.SITES_E2E_STATE) throw Error('Set SITES_LIVE_E2E=1 and SITES_E2E_STATE to opt into test-site changes and model usage.')
const statePath = process.env.SITES_E2E_STATE, s = JSON.parse(await readFile(statePath, 'utf8'))
const origin = process.env.DASHBOARD_ORIGIN || 'http://127.0.0.1:5173', platform = process.env.PLATFORM_URL || 'http://127.0.0.1:3100'
const browser = await chromium.launch(), page = await browser.newPage({ viewport: { width: 1440, height: 1000 } })
const errors = []; page.on('pageerror', error => errors.push(error.message))
const save = () => writeFile(statePath, JSON.stringify(s, null, 2))
async function waitFor(fn, description, timeout = 240_000) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) { if (await fn()) return; await new Promise(r => setTimeout(r, 1000)) }
  throw Error('Timed out: ' + description)
}
try {
  await page.goto(`${origin}/projects/${s.project}/${s.session}`)
  await page.getByRole('link', { name: 'View site', exact: true }).click()
  const name = page.getByLabel('Site name', { exact: true })
  await name.waitFor()
  if (await name.inputValue() !== 'Sites Step 7 E2E') {
    await name.fill('Sites Step 7 E2E')
    await page.getByRole('button', { name: 'Save name', exact: true }).click()
  }
  await page.getByRole('heading', { name: 'Sites Step 7 E2E', exact: true }).waitFor()
  const frame = page.frameLocator('iframe')
  await frame.getByRole('heading', { name: 'Sites E2E v1', exact: true }).waitFor()
  await waitFor(async () => /^\d+$/.test(await frame.locator('#count').innerText()), 'initial counter')
  const before = Number(await frame.locator('#count').innerText())
  await frame.getByRole('button', { name: 'Increment', exact: true }).click()
  await waitFor(async () => Number(await frame.locator('#count').innerText()) === before + 1, 'authenticated backend increment')
  s.counter = before + 1
  // Lose the POST reply after acceptance. Inspection must recover the snapshot
  // without resubmission, and clear the stale transport error.
  let posts = 0
  await page.route('**/api/site-view/projects/*/sites/*/snapshots', async route => {
    if (route.request().method() !== 'POST') return route.continue()
    posts++; s.snapshotOperation = route.request().postDataJSON().id; await save()
    assert.equal((await route.fetch({ headers: { ...(await route.request().allHeaders()), 'sec-fetch-site': 'same-origin', origin } })).status(), 200)
    await route.abort('connectionreset')
  })
  s.snapshotName = 'E2E saved counter ' + Date.now()
  await page.getByLabel('Snapshot name', { exact: true }).fill(s.snapshotName)
  await page.getByRole('button', { name: 'Save snapshot', exact: true }).click()
  await page.getByText('Snapshot saved.', { exact: true }).waitFor({ timeout: 15_000 })
  assert.equal(posts, 1)
  assert.equal(await page.getByRole('alert').count(), 0)
  await page.unroute('**/api/site-view/projects/*/sites/*/snapshots')
  console.log('PASS: backend bridge, rename, snapshot after lost reply')
  const edit = await browser.newPage()
  await edit.goto(origin + await page.getByRole('link', { name: 'Edit with Sites', exact: true }).getAttribute('href'))
  await edit.getByRole('button', { name: 'Site: Sites Step 7 E2E', exact: true }).waitFor()
  assert.equal(await edit.getByRole('button', { name: /^Environment:/ }).count(), 0)
  await edit.getByRole('button', { name: /^Model:/ }).click()
  await edit.getByText('GPT-5.6 Luna', { exact: true }).click()
  for (let i = 0; i < 3; i++) await edit.getByRole('button', { name: /^Reasoning:/ }).click()
  await edit.locator('textarea').fill('Sites Step 7 E2E edit: Read this existing site and change only the visible heading from "Sites E2E v1" to "Sites E2E v2" using tools.apply_patch. Preserve all other source and all data. Do not increment, create another site or take a snapshot. Read back the changed source and finish. Browser control is not implemented.')
  await edit.getByRole('button', { name: 'Send prompt', exact: true }).click()
  await edit.waitForURL(new RegExp(`/projects/${s.project}/[0-9a-f-]{36}$`))
  s.editSession = edit.url().split('/').at(-1); await save()
  await waitFor(async () => {
    const response = await fetch(`${platform}/api/sessions/${s.editSession}/runs`); assert.ok(response.ok)
    const run = (await response.json()).items[0]
    if (['failed', 'aborted'].includes(run?.status)) throw Error(`Edit run ${run.id}: ${run.status}`)
    if (run?.status === 'completed') { s.editRun = run.id; await save(); return true }
  }, 'second authoring run')
  await frame.getByRole('heading', { name: 'Sites E2E v2', exact: true }).waitFor({ timeout: 15_000 })
  assert.equal(Number(await frame.locator('#count').innerText()), s.counter)
  await waitFor(async () => await page.locator('.site-session-list a').count() >= 2, 'both authoring chats')
  await page.locator('.site-snapshot-list li').filter({ hasText: s.snapshotName }).getByRole('button', { name: 'Restore', exact: true }).click()
  await page.getByRole('group', { name: 'Confirm restore' }).getByRole('button', { name: 'Restore snapshot', exact: true }).click()
  await page.getByText('Snapshot restored. The restored code is live.', { exact: true }).waitFor()
  await frame.getByRole('heading', { name: 'Sites E2E v1', exact: true }).waitFor({ timeout: 15_000 })
  await waitFor(async () => Number(await frame.locator('#count').innerText()) === s.counter, 'counter survives code restore')
  assert.equal(await page.getByRole('alert').count(), 0); assert.deepEqual(errors, [])
  await page.screenshot({ path: '/tmp/sites-live-desktop.png', fullPage: true })
  await page.setViewportSize({ width: 780, height: 1000 })
  await page.screenshot({ path: '/tmp/sites-live-narrow.png', fullPage: true })
  s.uiPassed = true; await save()
  console.log('PASS: second authoring chat, live preview refresh, user restore, preserved data')
} finally { await browser.close() }
