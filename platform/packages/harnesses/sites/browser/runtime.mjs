import { createInterface } from 'node:readline';
import { BrowserHost } from './host.mjs';

const pending = new Map();
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const host = new BrowserHost((release_id, request) => new Promise((resolve, reject) => {
  const timer = setTimeout(() => { pending.delete(request.id); reject(new Error('Backend bridge timed out; accepted effects may continue.')); }, 25000);
  pending.set(request.id, { resolve, reject, timer });
  send({ kind: 'invoke', release_id, request });
}));
let closing = false;
async function close(exitCode = 0) {
  if (closing) return;
  closing = true;
  const forced = setTimeout(() => process.exit(1), 2500);
  await host.close().catch(() => {});
  clearTimeout(forced);
  process.exit(exitCode);
}

if (process.argv.includes('--check')) {
  try {
    await host.launch();
    await host.close();
  } catch (error) {
    console.error(error);
    process.exitCode = 1;
  }
} else {
  process.once('SIGTERM', () => close());
  process.once('SIGINT', () => close());
  process.once('uncaughtException', () => close(1));
  process.once('unhandledRejection', () => close(1));
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  let busy = false;
  let idle = setTimeout(close, 300000);
  input.on('close', close);
  input.on('line', async line => {
    if (line.length > 256 * 1024) { await close(); return; }
    let message;
    try { message = JSON.parse(line); } catch { await close(); return; }
    if (message.kind === 'backend_result') {
      const item = pending.get(message.id);
      if (item) {
        pending.delete(message.id); clearTimeout(item.timer);
        if (message.error) item.reject(new Error(message.error));
        else item.resolve(message.result);
      }
      return;
    }
    if (message.kind !== 'action' || busy || typeof message.id !== 'string') { await close(); return; }
    busy = true; clearTimeout(idle);
    try {
      const result = await host.execute(message.input, message.preview);
      send({ kind: 'result', id: message.id, result });
    } catch (error) {
      send({ kind: 'result', id: message.id, error: {
        code: error.code || 'BROWSER_EXECUTION_FAILED',
        message: String(error.message).slice(0, 2048),
        uncertain: true
      } });
    } finally { busy = false; idle = setTimeout(close, 300000); }
  });
}
