(() => {
  const host = globalThis.__codeHost;
  const input = JSON.parse(globalThis.__codeInput);
  delete globalThis.__codeHost;
  delete globalThis.__codeInput;
  const stringify = JSON.stringify.bind(JSON), parse = JSON.parse.bind(JSON);
  const freeze = Object.freeze.bind(Object), create = Object.create.bind(Object);
  const ErrorType = Error;
  const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
  const construct = Function;
  function deepFreeze(v) {
    if (v && typeof v === 'object') { for (const x of Object.values(v)) deepFreeze(x); freeze(v); }
    return v;
  }
  function rpc(message) {
    const reply = parse(host(stringify(message)));
    if (reply.error) {
      const error = new ErrorType(reply.error.message);
      error.code = reply.error.code;
      error.callId = reply.callId;
      error.uncertain = reply.error.uncertain === true;
      throw error;
    }
    return reply.value;
  }
  const tools = create(null);
  for (const tool of input.tools) {
    tools[tool.name] = async (args) => rpc({kind:'call', tool:tool.name, input:args ?? null});
  }
  const callTool = (name, args) => {
    if (!Object.hasOwn(tools, name)) throw new ErrorType('Tool is not registered');
    return tools[name](args);
  };
  const ctx = create(null);
  for (const extension of input.extensions) {
    ctx[extension.name] = construct('return (' + extension.factory + ')')()(callTool);
  }
  deepFreeze(ctx); freeze(tools);
  const text = value => rpc({kind:'text', value:value ?? null});
  const definitions = deepFreeze(input.tools);
  // Source gets only these capabilities. No persistent JS bindings cross cells.
  const run = new AsyncFunction('ctx','tools','text','ALL_TOOLS',input.source);
  return async function () {
    try {
      const value = await run(ctx,tools,text,definitions);
      return stringify({kind:'result',value:value ?? null});
    } catch (error) {
      // Bounded guest diagnostics; trusted transport errors were sanitized upstream.
      return stringify({kind:'failure',code:'JAVASCRIPT_ERROR',message:String(error?.message ?? error).slice(0,2048)});
    }
  };
})()
