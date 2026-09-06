-- Project credentials and grants are administered only by trusted operators.
create table site_project_access (
    project_id uuid primary key references projects(project_id) on delete restrict,
    token_hash text not null unique,
    enabled boolean not null default true
);
create table site_harness_grants (
    project_id uuid not null references projects(project_id) on delete restrict,
    harness_id text not null references harnesses(id) on delete restrict,
    environment_mode text not null check (environment_mode in ('none','single')),
    primary key(project_id,harness_id)
);
create table site_account_grants (
    project_id uuid not null references projects(project_id) on delete restrict,
    account_id uuid not null,
    primary key(project_id,account_id)
);
create table project_sites (
    id uuid primary key,
    project_id uuid not null references projects(project_id) on delete restrict,
    name text not null check (char_length(name) between 1 and 128 and name=btrim(name)),
    desired_status text not null default 'ready' check (desired_status in ('ready','suspended')),
    deleted_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique(project_id,id)
);
create index project_sites_project on project_sites(project_id,id);
-- Frozen forwarding requests survive ambiguous service responses. No secrets.
create table site_invocations (
    site_id uuid not null references project_sites(id) on delete restrict,
    id uuid not null,
    request jsonb not null,
    created_at timestamptz not null default now(),
    primary key(site_id,id)
);
-- Attribution commits in the same transaction as each SDK mutation.
create table site_runtime_operations (
    site_id uuid not null references project_sites(id) on delete restrict,
    invocation_id uuid not null,
    operation text not null,
    operation_key text not null,
    session_id uuid not null references sessions(id) on delete restrict,
    run_id uuid not null references runs(id) on delete restrict,
    created_at timestamptz not null default now(),
    primary key(site_id,operation,operation_key),
    foreign key(site_id,invocation_id) references site_invocations(site_id,id)
);

-- Site-scoped receipts share the runtime's atomic commit/replay machinery.
alter table runtime_requests drop constraint runtime_requests_scope_kind_check;
alter table runtime_requests add constraint runtime_requests_scope_kind_check
    check (scope_kind ~ '^(project|session|run|site)\.[a-z][a-z0-9_.]{0,127}$');
create or replace function runtime_check_request_scope() returns trigger language plpgsql as $$
begin
    case split_part(new.scope_kind, '.', 1)
        when 'project' then
            if new.scope_id <> new.project_id then raise exception 'request scope project mismatch' using errcode = '23503'; end if;
        when 'session' then
            perform 1 from sessions where id = new.scope_id and project_id = new.project_id for key share;
            if not found then raise exception 'request scope session not found in project' using errcode = '23503'; end if;
        when 'run' then
            perform 1 from runs where id = new.scope_id and project_id = new.project_id for key share;
            if not found then raise exception 'request scope run not found in project' using errcode = '23503'; end if;
        when 'site' then
            perform 1 from project_sites where id = new.scope_id and project_id = new.project_id for key share;
            if not found then raise exception 'request scope site not found in project' using errcode = '23503'; end if;
        else raise exception 'unsupported request scope' using errcode = '23514';
    end case;
    return new;
end;
$$;
