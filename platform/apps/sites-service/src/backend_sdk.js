// Executed before the site's module. Capture host bindings and intrinsics, then
// remove the bootstrap globals so site code only sees the documented SDK.
(() => {
  const host = globalThis.__sitesHost;
  const input = JSON.parse(globalThis.__sitesInput);
  delete globalThis.__sitesHost;
  delete globalThis.__sitesInput;
  const stringify = JSON.stringify.bind(JSON);
  const parse = JSON.parse.bind(JSON);
  const freeze = Object.freeze.bind(Object);
  const ErrorType = Error;
  const now = Date.now.bind(Date);
  let transaction = false;
  function rpc(method, args) {
    const answer = parse(host(stringify({ method, args })));
    if (answer.error) {
      const e = new ErrorType(answer.error.message);
      e.code = answer.error.code;
      throw e;
    }
    return answer.value;
  }
  function statement(sql, params) {
    if (typeof sql !== 'string' || !Array.isArray(params)) throw new ErrorType('Expected SQL and positional parameters');
    for (const value of params) {
      if (value !== null && typeof value !== 'string' &&
          !(typeof value === 'number' && Number.isFinite(value) && Math.abs(value) <= Number.MAX_SAFE_INTEGER)) {
        throw new ErrorType('SQL parameters must be null, strings, or finite safe numbers');
      }
    }
    return { sql, params };
  }
  function requireOutsideTransaction() {
    if (transaction) throw new ErrorType('Use the transaction handle; nested transactions and other capabilities are unavailable');
  }
  const db = freeze({
    async query(sql, params = []) { requireOutsideTransaction(); return rpc('query', statement(sql, params)); },
    async execute(sql, params = []) { requireOutsideTransaction(); return rpc('execute', statement(sql, params)); },
    async batch(statements) {
      requireOutsideTransaction();
      return rpc('batch', statements.map(s => statement(s.sql, s.params ?? [])));
    },
    async transaction(fn) {
      requireOutsideTransaction();
      if (typeof fn !== 'function') throw new ErrorType('Expected a transaction callback');
      rpc('begin', null);
      transaction = true;
      let active = true;
      const call = (method, sql, params) => {
        if (!active) throw new ErrorType('Transaction handle has expired');
        return rpc(method, statement(sql, params));
      };
      const tx = freeze({
        async query(sql, params = []) { return call('query', sql, params); },
        async execute(sql, params = []) { return call('execute', sql, params); }
      });
      try {
        const result = await fn(tx);
        rpc('commit', null);
        return result;
      } catch (error) {
        rpc('rollback', null);
        throw error;
      } finally { active = false; transaction = false; }
    }
  });
  const log = {};
  for (const level of ['info', 'warn', 'error']) {
    log[level] = (message, data = null) => rpc('log', { level, message, data });
  }
  const invocation = freeze({
    ...input.invocation,
    throwIfCancelled() {
      if (now() >= input.invocation.deadlineAt) throw new ErrorType('Invocation deadline reached');
      rpc('check', null);
    }
  });
  const callPlatform = async (method, args) => {
    requireOutsideTransaction();
    return rpc('platform.' + method, args);
  };
  const platform = freeze({
    callbacks: freeze({
      list: (options = {}) => callPlatform('callbacks.list', { options }),
      get: callbackId => callPlatform('callbacks.get', { callbackId }),
    }),
    environments: freeze({
      list: () => callPlatform('environments.list', {}),
      get: environmentId => callPlatform('environments.get', { environmentId }),
    }),
    harnesses: freeze({
      list: () => callPlatform('harnesses.list', {}),
      get: harnessId => callPlatform('harnesses.get', { harnessId }),
    }),
    sessions: freeze({
      startOptions: harnessId => callPlatform('sessions.startOptions', { harnessId }),
      start: (input, options) => callPlatform('sessions.start', { input, options }),
      list: (options = {}) => callPlatform('sessions.list', { options }),
      get: sessionId => callPlatform('sessions.get', { sessionId }),
      messages: (sessionId, options = {}) => callPlatform('sessions.messages', { sessionId, options }),
      metrics: (sessionId, options = {}) => callPlatform('sessions.metrics', { sessionId, options }),
      send: (sessionId, input, options) => callPlatform('sessions.send', { ...input, sessionId, options }),
      stop: (sessionId, options) => {
        const { expectedRunId, reason, idempotencyKey, ...unknown } = options;
        return callPlatform('sessions.stop', { ...unknown, sessionId, expectedRunId, reason, options: { idempotencyKey } });
      },
    }),
  });
  const ctx = freeze({ site: freeze(input.site), invocation, db, log: freeze(log), platform });
  // The runner is captured by Rust and removed before loading site code.
  return async function run(handler) {
    const response = await handler(input.request, ctx);
    if (transaction) throw new ErrorType('Backend returned with an unawaited transaction');
    return stringify(response);
  };
})()
