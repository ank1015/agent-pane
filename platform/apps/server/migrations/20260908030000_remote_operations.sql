-- Durable accepted intent and bounded process state, independent of agent leases.
alter table site_project_access add column execution_enabled boolean not null default false;
create table platform_remote_operations (
    id uuid primary key,
    project_id uuid not null references projects(project_id) on delete restrict,
    kind text not null check (kind in ('sandbox','execution')),
    environment_id uuid,
    host_id uuid,
    workspace jsonb not null check (jsonb_typeof(workspace)='object'),
    request jsonb not null check (jsonb_typeof(request)='object'),
    state jsonb not null default '{}' check (jsonb_typeof(state)='object'),
    status text not null,
    error jsonb,
    output text not null default '' check (octet_length(output)<=1048576),
    output_bytes bigint not null default 0 check (output_bytes>=0),
    truncated boolean not null default false,
    exit_code integer,
    cancel_requested boolean not null default false,
    cancellation_confirmed boolean not null default false,
    created_at timestamptz not null default clock_timestamp(),
    expires_at timestamptz not null,
    finished_at timestamptz,
    next_attempt_at timestamptz not null default clock_timestamp(),
    processor_id uuid,
    processor_expires_at timestamptz,
    process_epoch bigint not null default 0,
    source_run_id uuid,
    site_id uuid,
    invocation_id uuid,
    foreign key(project_id,source_run_id) references runs(project_id,id) on delete restrict,
    foreign key(project_id,site_id) references project_sites(project_id,id) on delete restrict,
    foreign key(site_id,invocation_id) references site_invocations(site_id,id),
    check ((source_run_id is not null and site_id is null and invocation_id is null)
        or (source_run_id is null and site_id is not null and invocation_id is not null)),
    check (status in ('provisioning','ready','unavailable','terminating','terminated','expired','pending','running','completed','failed','cancelled','lost')),
    unique(project_id,id)
);
create unique index platform_sandbox_host on platform_remote_operations(host_id) where kind='sandbox';
create index platform_remote_due on platform_remote_operations(next_attempt_at) where finished_at is null;
create index platform_remote_project on platform_remote_operations(project_id,id);
