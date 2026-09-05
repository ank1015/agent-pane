-- Before upgrading an existing database, run scripts/backfill-environment-roots.mjs.
-- Never silently replace an unresolved root ID with a guessed filesystem path.
do $$ begin
    if exists (select 1 from project_environments where workspace_root_path is null) then
        raise exception 'Unresolved environment roots: run scripts/backfill-environment-roots.mjs before migrating';
    end if;
end $$;

alter table project_environments drop constraint project_environments_workspace_root_check;
update project_environments set workspace_root = workspace_root_path;
alter table project_environments
    add constraint project_environments_workspace_root_check check (
        workspace_root = btrim(workspace_root)
        and octet_length(workspace_root) between 1 and 4096
        and (left(workspace_root, 1) = '/' or workspace_root ~ '^[A-Za-z]:[/\\]' or left(workspace_root, 2) = E'\\\\')
    ),
    drop column workspace_root_path;
comment on column project_environments.workspace_root is 'Absolute host-native workspace path; never an execution root ID';

-- New sessions advertise paths. Historical immutable session/run configurations
-- remain untouched and are translated privately by the basic harness.
update harnesses set config_schema = jsonb_set(jsonb_set(config_schema,
    '{properties,environment,oneOf,0,properties,workspace_root}',
    '{"type":"string","minLength":1,"maxLength":4096,"pattern":"^(/|[A-Za-z]:[/\\\\]|\\\\\\\\)","description":"Absolute native workspace path returned by discovery; not a root ID."}'::jsonb),
    '{properties,environment,oneOf,1,properties,workspace_root}',
    '{"type":"string","minLength":1,"maxLength":4096,"pattern":"^(/|[A-Za-z]:[/\\\\]|\\\\\\\\)","description":"Absolute native workspace path returned by discovery; not a root ID."}'::jsonb)
where id='basic-cc-tools-harness';
