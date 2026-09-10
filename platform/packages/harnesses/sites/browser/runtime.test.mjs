import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { BrowserHost, IMAGE_BYTES, validCall, backendResult } from './host.mjs';

const dashboard = 'http://127.0.0.1:1';
const sdk = (await readFile(new URL('../../../../apps/sites-service/src/frontend_sdk.js', import.meta.url), 'utf8'))
  .replace('__DASHBOARD_ORIGIN__', JSON.stringify(dashboard));
const release1 = '00000000-0000-0000-0000-000000000001';
const release2 = '00000000-0000-0000-0000-000000000002';

async function fixture(t, options) {
  const calls = [], requests = [];
  const server = createServer((req, res) => {
    requests.push(req.url);
    res.setHeader('content-type', 'text/html');
    if (!req.url.includes('/unsafe/')) res.setHeader('content-security-policy', `default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; worker-src 'none'; frame-ancestors ${dashboard}; sandbox allow-scripts`);
    res.end(`<!doctype html><script>${sdk}</script><h1>${req.url.includes(release2) ? 'Second' : 'First'}</h1><button onclick="this.textContent='Clicked'">Click</button><script>window.startup=callBackend('/startup').then(v => {window.started=v;});</script>`);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const preview = (release_id = release1) => ({ url: `${origin}/content/ticket/site/${release_id}/index.html`, release_id, dashboard_origin: dashboard, expires_at: Math.floor(Date.now() / 1000) + 60 });
  const host = new BrowserHost(async (release, request) => {
    calls.push({ release, request });
    if (request.endpoint === '/bad') return { status: 'succeeded', response: { status: 400, body: { error: 'Invalid task' } } };
    return { status: 'succeeded', response: { status: 200, body: { release, input: request.input ?? null } } };
  }, options);
  t.after(async () => { await host.close(); await new Promise(resolve => server.close(resolve)); });
  const evaluate = code => host.execute({ action: 'evaluate', code });
  return { host, preview, evaluate, calls, requests };
}

test('evaluate, state, production MessageChannel bridge, screenshot, and explicit latest-release reload', async t => {
  const { host, preview, evaluate, calls } = await fixture(t);
  assert.deepEqual(await host.execute({ action: 'reload' }, preview()), { action: 'reload', release_id: release1 });
  assert.deepEqual((await evaluate("await window.startup; return {title:document.querySelector('h1').textContent, started:window.started};")).result,
    { title: 'First', started: { release: release1, input: null } });
  assert.deepEqual(await evaluate('window.counter = 7;'), { action: 'evaluate', result: null });
  assert.equal((await evaluate('return ++window.counter;')).result, 8);
  assert.deepEqual((await evaluate("return await callBackend('/echo', {value:3});")).result, { release: release1, input: { value: 3 } });
  await assert.rejects(evaluate("return await callBackend('/bad');"), /Invalid task/);
  await evaluate("document.querySelector('button').click();");
  assert.equal((await evaluate("return document.querySelector('button').textContent;")).result, 'Clicked');
  await evaluate("document.body.style.background='#3399cc';");
  const shot = await host.execute({ action: 'screenshot' });
  assert.equal(shot.image.mimeType, 'image/jpeg');
  const bytes = Buffer.from(shot.image.data, 'base64');
  assert.equal(bytes.subarray(0, 2).toString('hex'), 'ffd8');
  assert.ok(bytes.length <= IMAGE_BYTES);
  assert.ok(Buffer.byteLength(JSON.stringify(shot)) < 128 * 1024);
  const pixel = await host.page.evaluate(async data => {
    const image = new Image(); image.src = 'data:image/jpeg;base64,' + data; await image.decode();
    const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height;
    const ctx = canvas.getContext('2d'); ctx.drawImage(image, 0, 0);
    return [...ctx.getImageData(1000, 700, 1, 1).data];
  }, shot.image.data);
  assert.ok(pixel.slice(0, 3).every((v, i) => Math.abs(v - [51, 153, 204][i]) <= 3), `Screenshot did not capture the site: ${pixel}`);
  assert.deepEqual((await evaluate('return [innerWidth, innerHeight];')).result, [1024, 768]);
  await host.execute({ action: 'reload' }, preview(release2));
  assert.deepEqual((await evaluate("await window.startup; return [document.querySelector('h1').textContent, typeof window.counter, window.started.release];")).result, ['Second', 'undefined', release2]);
  assert.deepEqual(calls.map(c => c.release), [release1, release1, release1, release2]);
});

test('page-only execution, JSON bounds, denied direct host binding, and network restrictions', async t => {
  const { host, preview, evaluate, calls, requests } = await fixture(t);
  await host.execute({ action: 'reload' }, preview());
  assert.deepEqual((await evaluate('return [typeof process, typeof require, typeof tools, typeof page];')).result, ['undefined', 'undefined', 'undefined', 'undefined']);
  for (const code of ['return document.body;', 'return () => 1;', 'return NaN;', 'return {a:undefined};', 'const a={};a.self=a;return a;', "return 'a'.repeat(100000);"]) {
    await assert.rejects(evaluate(code), /JSON|limit|96 KiB/);
  }
  await assert.rejects(evaluate("return __siteBackend({id:crypto.randomUUID(),endpoint:'/forbidden'});"), /rejected/);
  await assert.rejects(evaluate("return parent.document.body.innerHTML;"), /Blocked|SecurityError/);
  assert.equal((await evaluate("try { await fetch('http://127.0.0.1:1/private'); return false; } catch { return true; }")).result, true);
  await evaluate("const i=new Image();i.src='http://127.0.0.1:1/private';await new Promise(r=>{i.onerror=r;setTimeout(r,100);});");
  assert.ok(!calls.some(c => c.request.endpoint === '/forbidden'));
  assert.ok(requests.every(path => path.endsWith('/index.html')));
  await assert.rejects(host.execute({ action: 'evaluate', code: 'return 1', url: 'http://other' }), /Use evaluate/);
  await assert.rejects(host.execute({ action: 'screenshot', code: 'return 1' }), /Use evaluate/);
});

test('timeout destroys the page, explicit reload recovers, and unsafe/expired previews fail', async t => {
  const { host, preview, evaluate } = await fixture(t);
  await host.execute({ action: 'reload' }, preview());
  host.timeoutMs = 150;
  await assert.rejects(evaluate('await new Promise(() => {});'), error => error.code === 'BROWSER_TIMEOUT');
  await assert.rejects(evaluate('return 1;'), error => error.code === 'BROWSER_RESET_REQUIRED');
  host.timeoutMs = 30000;
  await host.execute({ action: 'reload' }, preview());
  assert.equal((await evaluate('return 2;')).result, 2);
  host.preview.expires_at = 0;
  await assert.rejects(host.execute({ action: 'screenshot' }), error => error.code === 'BROWSER_RESET_REQUIRED');
  const unsafe = preview(); unsafe.url = unsafe.url.replace('/content/', '/unsafe/');
  host.timeoutMs = 500;
  await assert.rejects(host.execute({ action: 'reload' }, unsafe));
});

test('bridge rejects malformed requests and preserves HTTP/backend failures', () => {
  const valid = { id: '00000000-0000-0000-0000-000000000001', endpoint: '/ok', input: null };
  assert.ok(validCall(valid));
  for (const endpoint of ['//other', '/bad?x=1', '/bad#x', '/bad\n', 'relative']) assert.ok(!validCall({ ...valid, endpoint }));
  assert.ok(!validCall({ ...valid, release_id: release2 }));
  assert.throws(() => backendResult({ status: 'failed' }), /execution failed/);
  assert.throws(() => backendResult({ status: 'succeeded', response: { status: 500, body: {} } }), /500/);
});

test('oversized screenshot fails without resizing or truncating media', async t => {
  const { host, preview, evaluate } = await fixture(t);
  await host.execute({ action: 'reload' }, preview());
  await evaluate(`
    document.body.style.margin='0';
    document.body.innerHTML='<canvas width="1024" height="768"></canvas>';
    const canvas=document.querySelector('canvas'), ctx=canvas.getContext('2d');
    const pixels=ctx.createImageData(1024,768); let seed=1234567;
    for(let i=0;i<pixels.data.length;i+=4) {
      for(let j=0;j<3;j++){seed^=seed<<13;seed^=seed>>>17;seed^=seed<<5;pixels.data[i+j]=seed&255;}
      pixels.data[i+3]=255;
    }
    ctx.putImageData(pixels,0,0);
  `);
  await assert.rejects(host.execute({ action: 'screenshot' }), error => error.code === 'BROWSER_IMAGE_LIMIT');
  assert.deepEqual((await evaluate('return [innerWidth,innerHeight];')).result, [1024, 768]);
});

test('pipe protocol services startup callbacks and exits cleanly on worker EOF', { timeout: 15000 }, async t => {
  const { preview } = await fixture(t);
  const child = spawn(process.execPath, [new URL('./runtime.mjs', import.meta.url).pathname], { stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => { if (child.exitCode === null) child.kill('SIGTERM'); });
  const exited = once(child, 'exit');
  const lines = createInterface({ input: child.stdout });
  const results = [], waiting = new Map();
  const send = v => child.stdin.write(JSON.stringify(v) + '\n');
  lines.on('line', line => {
    const frame = JSON.parse(line);
    if (frame.kind === 'invoke') send({ kind: 'backend_result', id: frame.request.id, result: { status: 'succeeded', response: { status: 200, body: { actualBridge: true } } } });
    else if (frame.kind === 'result') { results.push(frame); waiting.get(frame.id)?.(frame); }
  });
  const action = async (id, input, p) => {
    const ready = new Promise(resolve => waiting.set(id, resolve));
    send({ kind: 'action', id, input, preview: p });
    const result = await ready;
    assert.equal(result.error, undefined, JSON.stringify(result));
    return result.result;
  };
  await action('load', { action: 'reload' }, preview());
  assert.deepEqual(await action('read', { action: 'evaluate', code: 'await window.startup; return window.started;' }), { action: 'evaluate', result: { actualBridge: true } });
  const shot = await action('image', { action: 'screenshot' });
  assert.equal(shot.image.type, 'image');
  child.stdin.end();
  const [code] = await exited;
  assert.equal(code, 0);
  assert.equal(results.length, 3);
});
