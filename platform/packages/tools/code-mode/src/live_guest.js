(() => {
  const emit = globalThis.__liveEmit;
  const bridge = globalThis.__liveSync;
  const exit = globalThis.__liveExit;
  const input = JSON.parse(globalThis.__liveInput);
  delete globalThis.__liveEmit;
  delete globalThis.__liveSync;
  delete globalThis.__liveExit;
  delete globalThis.__liveInput;
  const stringify = JSON.stringify.bind(JSON), parse = JSON.parse.bind(JSON);
  const freeze = Object.freeze.bind(Object), create = Object.create.bind(Object);
  const ErrorType = Error;
  const pending = new Map(), timers = new Map();
  let next = 0, done = false;
  const send = frame => emit(stringify(frame));
  const serialize = value => typeof value === 'string' ? value : stringify(value) ?? String(value);
  const output = item => send({kind:'output', item});
  const text = value => output({type:'input_text', text:serialize(value)});
  function rpc(kind, fields) {
    const id = ++next;
    return new Promise((resolve, reject) => {
      pending.set(id, {resolve, reject});
      send({kind, id, ...fields});
    });
  }
  const tools = create(null), originals = create(null);
  for (const tool of input.tools) {
    const invoke = args => rpc('call', {tool:tool.original, input:args ?? {}});
    tools[tool.name] = invoke;
    originals[tool.original] = invoke;
  }
  freeze(tools);
  const ctx = create(null);
  for (const extension of input.extensions) {
    ctx[extension.name] = Function('return (' + extension.factory + ')')()((name,args) => {
      if (!Object.hasOwn(originals,name)) throw new ErrorType('Tool is not registered');
      return originals[name](args);
    });
  }
  freeze(ctx);
  const sync = frame => {
    const reply = parse(bridge(stringify(frame)));
    if (reply.error) throw new ErrorType(reply.error);
    return reply.value;
  };
  const store = (key,value) => {
    if (typeof key !== 'string') throw new ErrorType('Store key must be a string');
    sync({kind:'store',key,value:parse(stringify(value))});
  };
  const load = key => {
    if (typeof key !== 'string') throw new ErrorType('Store key must be a string');
    return sync({kind:'load',key});
  };
  function dataUrl(value, type) {
    if (typeof value !== 'string' || !value.startsWith('data:' + type + '/') || !value.includes(';base64,'))
      throw new ErrorType('Expected a base64 data URL');
    return value;
  }
  function image(value, override) {
    const url = typeof value === 'string' ? value : value.image_url ?? `data:${value.mimeType};base64,${value.data}`;
    const detail = override ?? value?.detail ?? value?._meta?.['codex/imageDetail'];
    if (detail != null && !['auto','low','high','original'].includes(detail)) throw new ErrorType('Invalid image detail');
    output({type:'input_image',image_url:dataUrl(url,'image'), ...(detail != null ? {detail} : {})});
  }
  function audio(value) {
    const url = typeof value === 'string' ? value : value.audio_url ?? `data:${value.mimeType};base64,${value.data}`;
    output({type:'input_audio',audio_url:dataUrl(url,'audio')});
  }
  function setTimeout(callback, delay = 0) {
    if (typeof callback !== 'function') throw new ErrorType('Timer callback must be a function');
    const id = ++next;
    timers.set(id, callback);
    pending.set(id, {resolve:() => { const cb = timers.get(id); timers.delete(id); if (cb) cb(); }, reject:() => {}});
    send({kind:'timer',id,delay:Math.max(0,Math.min(2147483647,Math.trunc(Number(delay)) || 0))});
    return id;
  }
  Object.assign(globalThis, {tools, ...(input.extensions.length ? {ctx} : {}), text, image, audio, store, load, setTimeout,
    clearTimeout:id => { if (timers.delete(id)) { pending.delete(id); send({kind:'clear_timer',id}); } },
    ALL_TOOLS:freeze(input.tools.map(({name,description}) => freeze({name,description}))),
    notify:value => send({kind:'notify',text:serialize(value)}),
    yield_control:() => rpc('yield',{}),
    generatedImage:value => { image(value.image_url); if (value.output_hint) text(value.output_hint); },
    exit
  });
  function failure(error) {
    done = true;
    send({kind:'failure',message:String(error?.message ?? error).slice(0,2048)});
  }
  return (kind,value) => {
    if (kind === 'done') return done;
    if (kind === 'failure') return failure(value);
    if (kind === 'start') {
      value.then(() => { done = true; send({kind:'done'}); }, failure);
    } else if (kind === 'reply') {
      const reply = parse(value), callback = pending.get(reply.id);
      if (!callback) return;
      pending.delete(reply.id);
      try {
        if (reply.error) {
          const error = new ErrorType(reply.error.message);
          Object.assign(error, reply.error);
          callback.reject(error);
        } else callback.resolve(reply.value);
      } catch(error) { failure(error); }
    }
  };
})()
