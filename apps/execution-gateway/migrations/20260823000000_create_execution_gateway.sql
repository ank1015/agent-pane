create table machine_registrations (
    id uuid primary key,
    token_hash bytea not null unique,
    label text,
    expires_at timestamptz not null,
    claimed_at timestamptz,
    created_at timestamptz not null default now()
);

create table machines (
    machine_id text primary key,
    environment_id text not null,
    name text not null,
    connector_kind text not null,
    descriptor jsonb not null,
    credential_hash bytea,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    last_seen_at timestamptz
);

create table machine_operations (
    id uuid primary key,
    machine_id text not null references machines(machine_id),
    status text not null,
    operation jsonb not null,
    response jsonb,
    error jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index machine_operations_machine_created_idx
    on machine_operations(machine_id, created_at desc);

create table machine_operation_events (
    operation_id uuid not null references machine_operations(id) on delete cascade,
    sequence bigint not null,
    item jsonb not null,
    created_at timestamptz not null default now(),
    primary key (operation_id, sequence)
);
