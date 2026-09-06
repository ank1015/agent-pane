// Injected by the content host before application code. Only the configured
// dashboard parent can attach the private MessageChannel; no Platform SDK here.
(() => {
  const parentOrigin = __DASHBOARD_ORIGIN__;
  const pending = new Map(); let port;
  let connect;
  const ready = new Promise(resolve => { connect = resolve; });
  const announce = () => parent.postMessage({ type: 'sites:ready:v1' }, parentOrigin);
  const announceTimer = setInterval(announce, 250);
  announce();
  window.addEventListener('message', event => {
    if (port || event.source !== parent || event.origin !== parentOrigin || event.data?.type !== 'sites:connect:v1' || event.ports.length !== 1) return;
    port = event.ports[0];
    port.onmessage = ({ data }) => {
      if (!data || typeof data.id !== 'string') return;
      const item = pending.get(data.id); if (!item) return;
      pending.delete(data.id); clearTimeout(item.timer);
      if (data.error) item.reject(Object.assign(new Error(data.error.message), { code: data.error.code }));
      else item.resolve(data.result);
    };
    clearInterval(announceTimer); port.start(); connect();
  });
  Object.defineProperty(window, 'callBackend', { configurable: false, writable: false, value: async (endpoint, input = null) => {
    if (typeof endpoint !== 'string' || !endpoint.startsWith('/') || endpoint.startsWith('//') || endpoint.length > 2048 || /[?#\x00-\x1f\x7f]/.test(endpoint)) throw new Error('Expected a local backend endpoint.');
    const id = crypto.randomUUID();
    const request = JSON.stringify({ id, endpoint, input });
    if (new TextEncoder().encode(request).length > 128 * 1024) throw new Error('Backend input exceeds 128 KiB.');
    if (pending.size >= 8) throw new Error('Too many backend calls.');
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(id); reject(new Error('Backend request timed out. Retry using the same application operation key.')); }, 45000);
      pending.set(id, { resolve, reject, timer });
      ready.then(() => { if (pending.has(id)) port.postMessage(JSON.parse(request)); });
    });
  }});
})();
