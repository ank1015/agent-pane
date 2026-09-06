// A small real site backend. Project permissions come from ctx, never inputs.
export default async function handler(request, ctx) {
  const input = request.body ?? {};
  switch (request.path) {
    case '/catalog': {
      const harnesses = await ctx.platform.harnesses.list();
      const environments = await ctx.platform.environments.list();
      return { status: 200, body: { harnesses: harnesses.items, environments: environments.items } };
    }
    case '/options':
      return { status: 200, body: await ctx.platform.sessions.startOptions(input.harnessId) };
    case '/sessions':
      return { status: 200, body: await ctx.platform.sessions.list({ limit: 30, ...(input.cursor ? { cursor: input.cursor } : {}) }) };
    case '/session':
      return { status: 200, body: {
        session: await ctx.platform.sessions.get(input.sessionId),
        messages: await ctx.platform.sessions.messages(input.sessionId, { limit: 50, afterRevision: input.afterRevision ?? 0 }),
        metrics: await ctx.platform.sessions.metrics(input.sessionId),
      } };
    case '/start': {
      if (typeof input.operationKey !== 'string') return { status: 400, body: { error: 'A stable operation key is required.' } };
      const accepted = await ctx.platform.sessions.start({
        harnessId: input.harnessId, environmentId: input.environmentId,
        model: input.model, accountId: input.accountId, prompt: input.prompt,
        options: input.options ?? {},
      }, { idempotencyKey: input.operationKey });
      return { status: 200, body: accepted };
    }
    default: return { status: 404, body: { error: 'Endpoint not found.' } };
  }
}
