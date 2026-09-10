import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createTestViteServer } from './vite-test-server.mjs'

const vite = await createTestViteServer()
after(() => vite.close())
const { MachinesSection } = await vite.ssrLoadModule('/src/features/machines/MachinesSection.tsx')
const { MachineActionDialog } = await vite.ssrLoadModule('/src/features/machines/MachineActionDialog.tsx')
const { machineKeys } = await vite.ssrLoadModule('/src/features/machines/machine-queries.ts')
const host = { id: '01900000-0000-7000-8000-000000000001', name: 'Test Mac', kind: 'registered', state: 'ready', descriptor: { operating_system: { type: 'macos' } } }
function render(component, props = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity, retry: false } } })
  client.setQueryData(machineKeys.inventory(), [host, { ...host, id: 'sandbox-id', name: 'Sandbox host', kind: 'e2b' }])
  client.setQueryData(machineKeys.accounts(), [])
  try { return renderToStaticMarkup(createElement(QueryClientProvider, { client }, createElement(component, props))) }
  finally { client.clear() }
}
test('registered machine cards expose accessible menu triggers, without sandbox host actions', () => {
  const html = render(MachinesSection)
  assert.match(html, /aria-label="Actions for Test Mac"/)
  assert.match(html, /aria-haspopup="menu"/)
  assert.match(html, /aria-expanded="false"/)
  assert.ok(!html.includes('Actions for Sandbox host'))
})
test('rename dialog starts with the existing name and disables an unchanged submission', () => {
  const html = render(MachineActionDialog, { host, action: 'rename', onClose() {} })
  assert.match(html, /Update name/)
  assert.match(html, /value="Test Mac"/)
  assert.match(html, /maxLength="200"/)
  assert.match(html, /disabled="">Confirm/)
})
test('delete confirmation uses a concise irreversible-action warning', () => {
  const html = render(MachineActionDialog, { host, action: 'delete', onClose() {} })
  assert.match(html, /Delete Test Mac/)
  assert.match(html, /This machine will be removed\. This action cannot be undone\./)
  assert.match(html, /Cancel/)
  assert.match(html, /delete-provider-confirm-button/)
})
