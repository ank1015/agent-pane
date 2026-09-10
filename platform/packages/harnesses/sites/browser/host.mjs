// Trusted browser operations. Model source is evaluated only in the sandboxed
// site frame, never in Node. This module has no Platform credentials.
import { chromium } from 'playwright';
import { randomUUID } from 'node:crypto';

export const VIEWPORT = Object.freeze({ width: 1024, height: 768 });
export const IMAGE_BYTES = 80 * 1024; // Base64 + envelope fits the 128 KiB tool limit.
const RESULT_BYTES = 96 * 1024;
const fail = (code, message) => Object.assign(new Error(message), { code });

export function validCall(v) {
  return v && typeof v === 'object' && !Array.isArray(v)
    && typeof v.id === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v.id)
    && typeof v.endpoint === 'string' && v.endpoint.startsWith('/') && !v.endpoint.startsWith('//')
    && v.endpoint.length <= 2048 && !/[?#\x00-\x1f\x7f]/.test(v.endpoint)
    && Object.keys(v).every(k => ['id', 'endpoint', 'input'].includes(k))
    && Buffer.byteLength(JSON.stringify(v)) <= 128 * 1024;
}

export function backendResult(v) {
  if (v?.status !== 'succeeded' || !v.response) {
    throw new Error(`Backend execution failed (${v?.error_code || v?.status || 'invalid response'}).`);
  }
  if (v.response.status < 200 || v.response.status >= 300) {
    throw new Error(typeof v.response.body?.error === 'string' ? v.response.body.error
      : typeof v.response.body?.message === 'string' ? v.response.body.message
      : `Backend returned ${v.response.status}.`);
  }
  if (v.responseTruncated) throw new Error('Backend response exceeded the bridge limit.');
  return v.response.body;
}

export class BrowserHost {
  constructor(invoke, { timeoutMs = 30000 } = {}) {
    this.invoke = invoke;
    this.timeoutMs = timeoutMs;
  }

  async launch() {
    // Native Chromium sandbox is mandatory, including in production containers.
    this.browser = await chromium.launch({ headless: true, chromiumSandbox: true });
  }

  async close() {
    await this.browser?.close();
    this.browser = undefined;
    this.context = undefined;
    this.frame = undefined;
  }

  async load(preview) {
    if (!preview || typeof preview.release_id !== 'string' || !Number.isFinite(preview.expires_at)) {
      throw fail('BROWSER_PREVIEW_INVALID', 'Invalid site preview configuration.');
    }
    const target = new URL(preview.url), dashboard = new URL(preview.dashboard_origin);
    const validOrigin = u => ['https:', 'http:'].includes(u.protocol) && !u.username && !u.password;
    if (!validOrigin(target) || !validOrigin(dashboard) || dashboard.origin === target.origin
      || dashboard.pathname !== '/' || dashboard.search || dashboard.hash || target.search || target.hash) {
      throw fail('BROWSER_PREVIEW_INVALID', 'Invalid site preview origins.');
    }
    await this.context?.close();
    if (!this.browser) await this.launch();
    this.frame = undefined;
    const context = this.context = await this.browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 1, acceptDownloads: false });
    const page = await context.newPage();
    context.on('page', other => { if (other !== page) void other.close().catch(() => {}); });
    page.on('dialog', dialog => { void dialog.dismiss().catch(() => {}); });
    const wrapper = `${dashboard.origin}/.sites-agent-preview/${randomUUID()}`;
    const prefix = target.pathname.slice(0, target.pathname.lastIndexOf('/') + 1);
    let valid = true, loads = 0, pending = 0;
    const seen = new Set();
    // This synthetic trusted document has the configured dashboard origin so
    // the unchanged production SDK's origin and WindowProxy checks still apply.
    await context.route('**/*', async route => {
      try {
        const request = route.request(), candidate = new URL(request.url());
        if (request.frame() === page.mainFrame() && request.url() === wrapper && request.method() === 'GET') {
          await route.fulfill({ contentType: 'text/html', body: '<!doctype html><style>html,body{margin:0;width:100%;height:100%;overflow:hidden}iframe{border:0;width:100%;height:100%;display:block}</style>' });
          return;
        }
        const navigation = request.isNavigationRequest();
        if (candidate.origin !== target.origin || !candidate.pathname.startsWith(prefix) || candidate.search
          || request.method() !== 'GET' || (navigation && (request.frame().parentFrame() !== page.mainFrame() || request.url() !== target.href))) {
          await route.abort(); return;
        }
        const response = await route.fetch({ maxRedirects: 0, timeout: 10000 });
        const directives = (response.headers()['content-security-policy'] || '').split(';').map(s => s.trim().toLowerCase());
        if (response.status() >= 300 || (navigation && (!directives.includes('sandbox allow-scripts') || !directives.includes("worker-src 'none'")))) {
          await route.abort(); return;
        }
        await route.fulfill({ response });
      } catch { await route.abort().catch(() => {}); }
    });
    await context.routeWebSocket('**/*', socket => socket.close());
    await page.exposeBinding('__siteBackend', async ({ frame }, data) => {
      // Playwright bindings also appear in child frames. Never trust a call's
      // claimed origin: only the actual trusted main-frame caller may use it.
      if (frame !== page.mainFrame() || frame.url() !== wrapper || !valid || !validCall(data)
        || pending >= 8 || seen.size >= 4096 || seen.has(data.id) || Date.now() >= preview.expires_at * 1000) {
        throw new Error('Backend bridge request rejected; reload if the preview expired.');
      }
      seen.add(data.id); pending++;
      try { return backendResult(await this.invoke(preview.release_id, data)); }
      finally { pending--; }
    });
    await page.goto(wrapper, { waitUntil: 'domcontentloaded', timeout: 15000 });
    page.on('framenavigated', frame => {
      if (frame.parentFrame() === page.mainFrame() && frame.url() !== 'about:blank' && ++loads > 1) valid = false;
    });
    await page.evaluate(url => {
      const iframe = document.createElement('iframe');
      iframe.sandbox = 'allow-scripts';
      iframe.referrerPolicy = 'no-referrer';
      let port, loads = 0;
      window.__siteReady = false;
      iframe.addEventListener('load', () => { if (++loads > 1) { port?.close(); window.__siteReady = false; } });
      window.addEventListener('message', event => {
        if (port || event.source !== iframe.contentWindow || event.origin !== 'null' || event.data?.type !== 'sites:ready:v1') return;
        const channel = new MessageChannel(); port = channel.port1;
        port.onmessage = async ({ data }) => {
          if (!data || typeof data.id !== 'string' || data.id.length > 64) return;
          try { port.postMessage({ id: data.id, result: await window.__siteBackend(data) }); }
          catch (error) { port.postMessage({ id: data.id, error: { message: String(error.message).slice(0, 2048) } }); }
        };
        port.start();
        iframe.contentWindow.postMessage({ type: 'sites:connect:v1' }, '*', [channel.port2]);
        window.__siteReady = true;
      });
      iframe.src = url; document.body.append(iframe);
    }, target.href);
    await page.waitForFunction(() => window.__siteReady, null, { timeout: 15000 });
    const element = await page.locator('iframe').elementHandle();
    this.frame = await element.contentFrame();
    if (!this.frame || this.frame.url() !== target.href) throw fail('BROWSER_LOAD_FAILED', 'The site preview did not load.');
    await this.frame.waitForLoadState('load', { timeout: 15000 });
    this.page = page;
    this.preview = preview;
    this.valid = () => valid && !page.isClosed() && !this.frame.isDetached() && this.frame.url() === target.href;
  }

  async execute(input, preview) {
    let timer;
    try {
      return await Promise.race([
        this.perform(input, preview),
        new Promise((_, reject) => { timer = setTimeout(() => reject(fail('BROWSER_TIMEOUT', 'Browser action timed out. Page state is lost; use reload. Accepted backend effects may continue.')), this.timeoutMs); })
      ]);
    } catch (error) {
      if (error.code === 'BROWSER_TIMEOUT') await this.close();
      throw error;
    } finally { clearTimeout(timer); }
  }

  async perform(input, preview) {
    if (!input || !['evaluate', 'screenshot', 'reload'].includes(input.action)
      || Object.keys(input).some(k => !['action', ...(input.action === 'evaluate' ? ['code'] : [])].includes(k))
      || (input.action === 'evaluate' && (typeof input.code !== 'string' || !input.code.trim() || Buffer.byteLength(input.code) > 48 * 1024))) {
      throw fail('INVALID_INPUT', 'Use evaluate with code, screenshot, or reload.');
    }
    if (preview) await this.load(preview);
    if (!this.frame || !this.valid() || Date.now() >= this.preview.expires_at * 1000) {
      throw fail('BROWSER_RESET_REQUIRED', 'Preview is unavailable, navigated away, or expired. Use reload.');
    }
    if (input.action === 'reload') return { action: 'reload', release_id: this.preview.release_id };
    if (input.action === 'screenshot') {
      // Never resize or truncate an image. Try bounded JPEG quality, then fail
      // explicitly if this viewport cannot fit the existing media transport.
      for (const quality of [80, 60, 40]) {
        const bytes = await this.page.screenshot({ type: 'jpeg', quality, timeout: 10000 });
        if (bytes.length <= IMAGE_BYTES) return { action: 'screenshot', image: { type: 'image', mimeType: 'image/jpeg', data: bytes.toString('base64') } };
      }
      throw fail('BROWSER_IMAGE_LIMIT', 'Screenshot exceeds 80 KiB at supported JPEG quality. Simplify the visible content or scroll to a less complex area.');
    }
    // Validate before Playwright can turn DOM nodes/unsupported values into
    // undefined. Capture built-ins before running the supplied function body.
    const encoded = await this.frame.evaluate(`(async () => {
      const stringify = JSON.stringify.bind(JSON), proto = Object.getPrototypeOf.bind(Object),
        entries = Object.entries.bind(Object), array = Array.isArray.bind(Array), finite = Number.isFinite.bind(Number),
        objectPrototype = Object.prototype, Encoder = TextEncoder;
      const result = await (async () => {\n${input.code}\n})();
      let count = 0;
      function check(v, depth = 0) {
        if (++count > 20000 || depth > 64) throw new Error('Result exceeds JSON structure limits.');
        if (v === null || typeof v === 'string' || typeof v === 'boolean') return;
        if (typeof v === 'number' && finite(v)) return;
        if (typeof v !== 'object' || (!array(v) && proto(v) !== objectPrototype && proto(v) !== null)) throw new Error('Return only JSON values, not DOM nodes, handles, functions, or non-finite numbers.');
        for (const [, item] of entries(v)) check(item, depth + 1);
      }
      const value = result === undefined ? null : result;
      check(value); const json = stringify(value);
      if (new Encoder().encode(json).length > ${RESULT_BYTES}) throw new Error('Browser result exceeds 96 KiB. Return a smaller selection.');
      return json;
    })()`);
    if (typeof encoded !== 'string' || Buffer.byteLength(encoded) > RESULT_BYTES) throw fail('BROWSER_RESULT_LIMIT', 'Invalid or oversized browser result.');
    return { action: 'evaluate', result: JSON.parse(encoded) };
  }
}
