alter table snapshots
    add column sandbox_account_id uuid references sandbox_accounts(id) on delete restrict;

update snapshots snapshot
set sandbox_account_id = (
    select candidate.id
    from sandbox_accounts candidate
    where candidate.provider = snapshot.provider
      and candidate.deleted_at is null
      and candidate.enabled
    order by candidate.is_default desc, candidate.created_at, candidate.id
    limit 1
);

do $$
begin
    if exists (select 1 from snapshots where sandbox_account_id is null) then
        raise exception
            'every existing snapshot needs an enabled sandbox account for its provider before this migration can run';
    end if;
end $$;

alter table snapshots
    alter column sandbox_account_id set not null,
    drop constraint snapshots_provider_snapshot_id_unique,
    drop column provider,
    add constraint snapshots_account_snapshot_id_unique
        unique (sandbox_account_id, provider_snapshot_id);

create index snapshots_account_sandbox_created_idx
    on snapshots(sandbox_account_id, sandbox_id, created_at desc, id);

create table sandbox_environment_templates (
    id uuid primary key,
    name text not null,
    snapshot_id uuid not null references snapshots(id) on delete restrict,
    cwd text not null,
    creation_script text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,

    constraint sandbox_environment_templates_name_not_blank
        check (name = btrim(name) and name <> ''),
    constraint sandbox_environment_templates_cwd_absolute
        check (cwd like '/%')
);

create unique index sandbox_environment_templates_active_name_idx
    on sandbox_environment_templates(lower(name))
    where deleted_at is null;

create index sandbox_environment_templates_snapshot_id_idx
    on sandbox_environment_templates(snapshot_id);

create trigger sandbox_environment_templates_set_updated_at
before update on sandbox_environment_templates
for each row execute function execution_gateway_set_updated_at();

create table sandbox_environment_instances (
    environment_id text primary key references environments(environment_id) on delete restrict,
    template_id uuid not null references sandbox_environment_templates(id) on delete restrict,
    created_at timestamptz not null default now()
);

create index sandbox_environment_instances_template_created_idx
    on sandbox_environment_instances(template_id, created_at, environment_id);
