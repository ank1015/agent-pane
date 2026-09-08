// The same bundled helper works in agent cells and site handlers.
/** @param {import('../platform').Platform} platform */
async function inspectProject(platform) {
  const page = await platform.environments.list({limit: 1});
  if (!page.items.length) return null;
  const environment = await platform.environments.get(page.items[0].id);
  return {id: environment.id, name: environment.name, workspaceRoot: environment.workspace_root};
}

/**
 * @param {import('../platform').Platform} platform
 * @param {import('../platform').CreateSession} input
 * @param {string} key
 */
async function createTrial(platform, input, key) {
  return await platform.sessions.create(input, {idempotencyKey: key});
}
