import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'

const root = fileURLToPath(new URL('..', import.meta.url))
const src = fileURLToPath(new URL('../src', import.meta.url))

export function createTestViteServer() {
  return createServer({
    configFile: false,
    root,
    resolve: { alias: { '@': src } },
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { middlewareMode: true, ws: false },
    appType: 'custom',
  })
}
