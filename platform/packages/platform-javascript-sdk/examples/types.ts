import type {Platform, AgentPlatform, Account, Environment, CreateSession} from '../platform';
import type {SitePlatform} from '../../../apps/sites-service/src/sdk';
declare const agent: AgentPlatform;
declare const site: SitePlatform;
declare const input: CreateSession;
async function common(platform: Platform) {
  const accounts: Account[] = (await platform.accounts.list()).items;
  const environments: Environment[] = (await platform.environments.list()).items;
  const session = await platform.sessions.create(input, {idempotencyKey: 'shared-key'});
  return {accounts, environments, sessionId: session.session.id};
}
void common(agent);
void common(site);
void agent.sessions.create(input);
// @ts-expect-error A backend mutation requires an explicit stable key.
void site.sessions.create(input);

import type {AgentSitesContext} from '../sites';
async function editSite(ctx: AgentSitesContext) {
  const source = await ctx.sites.read();
  const frontend: string = source.files.frontend;
  await ctx.sites.applyPatch({patch: frontend});
  await ctx.sites.execute({sql: 'CREATE TABLE notes(id TEXT PRIMARY KEY)'});
  // @ts-expect-error Snapshots are exclusively user-controlled.
  ctx.sites.snapshots;
  // @ts-expect-error The agent does not supply an expected version.
  await ctx.sites.applyPatch({patch: '', expectedVersion: 1});
}
