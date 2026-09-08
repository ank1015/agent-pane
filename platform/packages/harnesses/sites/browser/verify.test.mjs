import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
test('probe requires native content isolation and reports genuine page errors', async () => {
  const server = createServer((req, res) => {
    res.setHeader('Content-Type', 'text/html');
    if (req.url !== '/unsafe/index.html') res.setHeader('Content-Security-Policy', "default-src 'none'; script-src 'unsafe-inline'; worker-src 'none'; sandbox allow-scripts");
    res.end(`<h1>Probe</h1>${req.url === '/error/index.html' ? '<script>throw Error("site defect")</script>' : ''}`);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  async function probe(path) {
    const child = spawn(process.execPath, [new URL('./verify.mjs', import.meta.url).pathname], { stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = ''; child.stdout.on('data', data => stdout += data); child.stderr.resume();
    const done = new Promise((resolve, reject) => { child.on('error', reject); child.on('exit', code => resolve({ code, stdout })); });
    child.stdin.end(JSON.stringify({ url: `http://127.0.0.1:${server.address().port}/${path}/index.html`, selectors: ['h1'] }));
    return done;
  }
  try {
    const valid = await probe('valid'); assert.equal(valid.code, 0);
    const result = JSON.parse(valid.stdout); assert.deepEqual(result.errors, []); assert.equal(result.checks[0].visible, true);
    const faulty = await probe('error'); assert.equal(faulty.code, 0); assert.ok(JSON.parse(faulty.stdout).errors.includes('site defect'));
    assert.notEqual((await probe('unsafe')).code, 0);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
