create table registered_hosts (
    host_id uuid primary key references execution_hosts(id) on delete restrict,
    installation_id uuid unique,
    registration_token_hash bytea,
    registration_token_expires_at timestamptz,
    registration_token_used_at timestamptz,
    credential_hash bytea,
    credential_created_at timestamptz,
    credential_revoked_at timestamptz,
    daemon_version text,
    protocol_version integer,
    daemon_instance_id uuid,
    connection_id uuid,
    registered_at timestamptz,
    last_connected_at timestamptz,
    last_disconnected_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint registered_hosts_registration_pair check (
        (registration_token_hash is null and registration_token_expires_at is null)
        or (registration_token_hash is not null and registration_token_expires_at is not null)
    ),
    constraint registered_hosts_credential_pair check (
        (credential_hash is null and credential_created_at is null)
        or (credential_hash is not null and credential_created_at is not null)
    ),
    constraint registered_hosts_protocol_positive check (
        protocol_version is null or protocol_version > 0
    )
);

create unique index registered_hosts_registration_token_idx
    on registered_hosts (registration_token_hash)
    where registration_token_hash is not null;

create trigger registered_hosts_set_updated_at
before update on registered_hosts
for each row execute function execution_gateway_set_updated_at();
