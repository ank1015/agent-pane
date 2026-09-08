// Trusted, one-shot frontend probe. No credentials or model JavaScript are evaluated in Node.
import { chromium } from 'playwright';
let browser;
process.once('SIGTERM', async () => {
  setTimeout(() => process.exit(1), 3000).unref();
  await browser?.close();
  process.exit(1);
});
const watchdog = setTimeout(async () => { await browser?.close(); process.exit(1); }, 22000);
try {
  let input = '';
  for await (const chunk of process.stdin) {
    input += chunk;
    if (input.length > 16384) throw new Error('Input limit');
  }
  const { url, selectors = [] } = JSON.parse(input);
  const target = new URL(url);
  if (!['http:', 'https:'].includes(target.protocol) || selectors.length > 20) throw new Error('Invalid input');
  // Chromium's native sandbox is required; never add --no-sandbox.
  browser = await chromium.launch({ headless: true, chromiumSandbox: true });
  const context = await browser.newContext({ acceptDownloads: false });
  const page = await context.newPage();
  const errors = [], blocked = [];
  const prefix = target.pathname.slice(0, target.pathname.lastIndexOf('/') + 1);
  await context.route('**/*', async route => {
    const request = route.request(), candidate = new URL(request.url());
    if (candidate.origin === target.origin && candidate.pathname.startsWith(prefix) && request.method() === 'GET') {
      // The service supplies an opaque-origin sandbox and blocks all workers.
      // Check before loading: Playwright's serviceWorkers:block injects a getter
      // that itself throws on these opaque origins, creating false page errors.
      const response = await route.fetch({ maxRedirects: 0 });
      if (request.isNavigationRequest()) {
        const directives = (response.headers()['content-security-policy'] || '').split(';').map(s => s.trim().toLowerCase());
        if (!directives.includes('sandbox allow-scripts') || !directives.includes("worker-src 'none'")) {
          if (blocked.length < 20) blocked.push({ reason: 'Missing required content sandbox policy' });
          await route.abort(); return;
        }
      }
      await route.fulfill({ response });
    } else {
      if (blocked.length < 20) blocked.push({ origin: candidate.origin, path: candidate.pathname.slice(0, 512) });
      await route.abort();
    }
  });
  // Deny websocket egress too; no backend/user-authentication bridge is installed.
  await context.routeWebSocket('**/*', socket => socket.close());
  page.on('pageerror', error => { if (errors.length < 20) errors.push(String(error.message).slice(0, 1000)); });
  const response = await page.goto(url, { waitUntil: 'load', timeout: 12000 });
  await page.waitForTimeout(300);
  const checks = [];
  for (const selector of selectors) {
    if (typeof selector !== 'string' || selector.length > 256) throw new Error('Invalid selector');
    try {
      const locator = page.locator(selector);
      const count = await locator.count();
      checks.push({ selector, count, visible: count > 0 && await locator.first().isVisible() });
    } catch { checks.push({ selector, error: 'Invalid or unsupported selector' }); }
  }
  process.stdout.write(JSON.stringify({ status: response?.status(), title: (await page.title()).slice(0, 1024), text: (await page.locator('body').innerText({ timeout: 2000 })).slice(0, 6000), checks, errors, blocked, scope: 'Frontend DOM only. External resources blocked. Backend bridge unavailable; use sites.invoke separately.' }));
} catch {
  process.exitCode = 1;
} finally {
  clearTimeout(watchdog);
  await browser?.close();
}
