create function execution_gateway_set_updated_at()
returns trigger
language plpgsql
as $$
begin
    new.updated_at = now();
    return new;
end;
$$;

create table e2b_accounts (
    id uuid primary key,
    name text not null,
    credential_ciphertext bytea not null,
    credential_nonce bytea not null,
    credential_key_version integer not null default 1,
    credential_fingerprint text not null,
    is_default boolean not null default false,
    status text not null default 'active',
    last_verified_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint e2b_accounts_name_valid check (name = btrim(name) and name <> ''),
    constraint e2b_accounts_ciphertext_not_empty check (octet_length(credential_ciphertext) > 0),
    constraint e2b_accounts_nonce_not_empty check (octet_length(credential_nonce) > 0),
    constraint e2b_accounts_key_version_positive check (credential_key_version > 0),
    constraint e2b_accounts_status_valid check (status in ('active', 'invalid', 'disabled')),
    constraint e2b_accounts_default_active check (not is_default or status = 'active')
);

create unique index e2b_accounts_name_idx on e2b_accounts (lower(name));
create unique index e2b_accounts_one_default_idx on e2b_accounts ((true)) where is_default;

create trigger e2b_accounts_set_updated_at
before update on e2b_accounts
for each row execute function execution_gateway_set_updated_at();

create table execution_hosts (
    id uuid primary key,
    kind text not null,
    name text,
    desired_state text not null,
    observed_state text not null,
    status_code text,
    status_message text,
    status_retryable boolean not null default false,
    descriptor jsonb,
    supervisor_generation_id uuid,
    metadata jsonb not null default '{}'::jsonb,
    last_seen_at timestamptz,
    reconcile_after timestamptz,
    reconcile_attempts integer not null default 0,
    reconcile_lease_owner text,
    reconcile_lease_until timestamptz,
    revision bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,

    constraint execution_hosts_kind_valid check (kind in ('e2b', 'registered')),
    constraint execution_hosts_name_valid check (name is null or (name = btrim(name) and name <> '')),
    constraint execution_hosts_desired_state_valid check (desired_state in ('ready', 'paused', 'deleted')),
    constraint execution_hosts_observed_state_valid check (observed_state in (
        'provisioning', 'ready', 'pausing', 'paused', 'resuming',
        'unavailable', 'deleting', 'deleted', 'failed', 'lost'
    )),
    constraint execution_hosts_metadata_object check (jsonb_typeof(metadata) = 'object'),
    constraint execution_hosts_attempts_nonnegative check (reconcile_attempts >= 0),
    constraint execution_hosts_revision_positive check (revision > 0)
);

create index execution_hosts_state_idx
    on execution_hosts (observed_state, desired_state) where deleted_at is null;
create index execution_hosts_reconcile_idx
    on execution_hosts (reconcile_after) where reconcile_after is not null;

create trigger execution_hosts_set_updated_at
before update on execution_hosts
for each row execute function execution_gateway_set_updated_at();

create table e2b_snapshots (
    id uuid primary key,
    e2b_account_id uuid not null references e2b_accounts(id) on delete restrict,
    e2b_snapshot_id text,
    source_host_id uuid references execution_hosts(id) on delete set null,
    name text,
    desired_state text not null default 'ready',
    observed_state text not null default 'creating',
    status_code text,
    status_message text,
    metadata jsonb not null default '{}'::jsonb,
    reconcile_after timestamptz,
    reconcile_attempts integer not null default 0,
    reconcile_lease_owner text,
    reconcile_lease_until timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,

    constraint e2b_snapshots_provider_id_valid check (
        e2b_snapshot_id is null or (e2b_snapshot_id = btrim(e2b_snapshot_id) and e2b_snapshot_id <> '')
    ),
    constraint e2b_snapshots_name_valid check (name is null or (name = btrim(name) and name <> '')),
    constraint e2b_snapshots_desired_state_valid check (desired_state in ('ready', 'deleted')),
    constraint e2b_snapshots_observed_state_valid check (observed_state in ('creating', 'ready', 'deleting', 'deleted', 'failed')),
    constraint e2b_snapshots_metadata_object check (jsonb_typeof(metadata) = 'object'),
    constraint e2b_snapshots_attempts_nonnegative check (reconcile_attempts >= 0)
);

create unique index e2b_snapshots_provider_id_idx
    on e2b_snapshots (e2b_account_id, e2b_snapshot_id)
    where e2b_snapshot_id is not null;
create index e2b_snapshots_account_idx on e2b_snapshots (e2b_account_id, created_at desc);
create index e2b_snapshots_source_idx on e2b_snapshots (source_host_id, created_at desc);
create index e2b_snapshots_reconcile_idx
    on e2b_snapshots (reconcile_after) where reconcile_after is not null;

create trigger e2b_snapshots_set_updated_at
before update on e2b_snapshots
for each row execute function execution_gateway_set_updated_at();

create table e2b_hosts (
    host_id uuid primary key references execution_hosts(id) on delete restrict,
    e2b_account_id uuid not null references e2b_accounts(id) on delete restrict,
    e2b_sandbox_id text,
    source_type text not null,
    source_snapshot_id uuid references e2b_snapshots(id) on delete restrict,
    timeout_seconds bigint not null,
    provider_state text,
    creation_outcome_ambiguous boolean not null default false,
    last_provider_check_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint e2b_hosts_sandbox_id_valid check (
        e2b_sandbox_id is null or (e2b_sandbox_id = btrim(e2b_sandbox_id) and e2b_sandbox_id <> '')
    ),
    constraint e2b_hosts_source_type_valid check (source_type in ('base', 'snapshot')),
    constraint e2b_hosts_source_consistent check (
        (source_type = 'base' and source_snapshot_id is null)
        or (source_type = 'snapshot' and source_snapshot_id is not null)
    ),
    constraint e2b_hosts_timeout_valid check (timeout_seconds between 1 and 86400)
);

create unique index e2b_hosts_provider_id_idx
    on e2b_hosts (e2b_account_id, e2b_sandbox_id)
    where e2b_sandbox_id is not null;
create index e2b_hosts_account_idx on e2b_hosts (e2b_account_id);

create trigger e2b_hosts_set_updated_at
before update on e2b_hosts
for each row execute function execution_gateway_set_updated_at();

create table idempotency_records (
    id uuid primary key,
    scope text not null,
    idempotency_key text not null,
    request_hash text not null,
    state text not null,
    resource_kind text,
    resource_id uuid,
    response_status integer,
    response_body jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    expires_at timestamptz not null,

    constraint idempotency_records_scope_valid check (scope = btrim(scope) and scope <> ''),
    constraint idempotency_records_key_valid check (idempotency_key = btrim(idempotency_key) and idempotency_key <> ''),
    constraint idempotency_records_state_valid check (state in ('in_progress', 'completed', 'failed')),
    constraint idempotency_records_resource_kind_valid check (
        resource_kind is null or resource_kind in ('host', 'snapshot')
    )
);

create unique index idempotency_records_scope_key_idx
    on idempotency_records (scope, idempotency_key);
create index idempotency_records_expiry_idx on idempotency_records (expires_at);

create trigger idempotency_records_set_updated_at
before update on idempotency_records
for each row execute function execution_gateway_set_updated_at();
