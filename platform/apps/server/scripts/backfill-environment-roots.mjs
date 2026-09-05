// Preflight for 20260906000000_environment_native_paths.sql.
// Resolves legacy IDs through read-only gateway requests, then records verified
// native paths in one short transaction. Never provisions/resumes sandbox compute.
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

process.loadEnvFile(fileURLToPath(new URL('../.env', import.meta.url)))
const database = new URL(process.env.PLATFORM_SERVER_DATABASE_URL)
const env = { ...process.env, PGHOST: database.hostname, PGPORT: database.port || '5432', PGUSER: decodeURIComponent(database.username), PGPASSWORD: decodeURIComponent(database.password), PGDATABASE: database.pathname.slice(1) }
function sql(query) {
  const result = spawnSync('psql', ['-X', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], { env, input: query, encoding: 'utf8' })
  if (result.status !== 0) throw new Error(result.stderr || 'Database request failed')
  return result.stdout.trim()
}
const quote = value => "'" + value.replaceAll("'", "''") + "'"
const legacy = sql("select count(*) from information_schema.columns where table_schema='public' and table_name='project_environments' and column_name='workspace_root_path';")
if (legacy === '0') {
  console.log('Native-path migration already applied; no backfill needed.')
  process.exit(0)
}
const rows = JSON.parse(sql("select coalesce(json_agg(e),'[]'::json) from (select id,type,machine_id,snapshot_id,workspace_root,updated_at from project_environments where workspace_root_path is null) e;"))
const base = process.env.PLATFORM_SERVER_EXECUTION_GATEWAY_URL.replace(/\/$/, '')
async function get(path) {
  const response = await fetch(base + path, { headers: { Authorization: 'Bearer ' + process.env.PLATFORM_SERVER_EXECUTION_GATEWAY_TOKEN }, redirect: 'error', signal: AbortSignal.timeout(30_000) })
  if (!response.ok) throw new Error(`Cannot resolve ${path}: HTTP ${response.status}; no changes applied`)
  return response.json()
}
const updates = []
for (const row of rows) {
  const hostId = row.type === 'machine' ? row.machine_id : (await get('/v1/snapshots/' + row.snapshot_id)).source_host_id
  if (!hostId) throw new Error(`Environment ${row.id} has no source host. Resolve its path explicitly before migrating.`)
  const host = await get('/v1/hosts/' + hostId)
  const roots = (host.descriptor?.roots ?? []).filter(root => root.id === row.workspace_root)
  if (roots.length !== 1 || !roots[0].native_path) throw new Error(`Environment ${row.id} has no unambiguous known root; no changes applied`)
  updates.push({ ...row, nativePath: roots[0].native_path })
}
if (updates.length) {
  sql('begin; set local lock_timeout = \'5s\'; set local standard_conforming_strings = on;\n' + updates.map(row => `with changed as (
    update project_environments set workspace_root_path=${quote(row.nativePath)} where id=${quote(row.id)} and updated_at=${quote(row.updated_at)}::timestamptz and workspace_root_path is null returning id
  ) select 1 / count(*) from changed;`).join('\n') + '\ncommit;')
}
console.log(`Verified and backfilled ${updates.length} legacy environment root paths. Ready for migration.`)
