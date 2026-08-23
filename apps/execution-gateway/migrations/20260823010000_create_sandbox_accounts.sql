create table execution_vault_secrets (
    id uuid primary key,
    encrypted_payload bytea not null,
    nonce bytea not null,
    encryption_key_version integer not null default 1,
    credential_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint execution_vault_secrets_payload_not_empty
        check (octet_length(encrypted_payload) > 0),
    constraint execution_vault_secrets_nonce_not_empty
        check (octet_length(nonce) > 0),
    constraint execution_vault_secrets_key_version_positive
        check (encryption_key_version > 0),
    constraint execution_vault_secrets_credential_version_positive
        check (credential_version > 0)
);

create table sandbox_accounts (
    id uuid primary key,
    provider text not null,
    name text not null,
    secret_id uuid,
    config jsonb not null default '{}'::jsonb,
    enabled boolean not null default true,
    is_default boolean not null default false,
    validation_status text not null default 'unchecked',
    last_validated_at timestamptz,
    last_validation_error jsonb,
    deleted_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint sandbox_accounts_secret_id_unique unique (secret_id),
    constraint sandbox_accounts_secret_id_fkey
        foreign key (secret_id) references execution_vault_secrets (id) on delete restrict,
    constraint sandbox_accounts_provider_supported
        check (provider in ('e2b', 'daytona', 'blaxel', 'tensorlake')),
    constraint sandbox_accounts_name_not_blank
        check (name = btrim(name) and name <> ''),
    constraint sandbox_accounts_config_is_object
        check (jsonb_typeof(config) = 'object'),
    constraint sandbox_accounts_default_is_enabled
        check (not is_default or enabled),
    constraint sandbox_accounts_validation_status_valid
        check (validation_status in ('unchecked', 'valid', 'invalid')),
    constraint sandbox_accounts_validation_error_is_object
        check (last_validation_error is null or jsonb_typeof(last_validation_error) = 'object'),
    constraint sandbox_accounts_active_has_secret
        check (deleted_at is not null or secret_id is not null),
    constraint sandbox_accounts_deleted_is_inactive
        check (deleted_at is null or (not enabled and not is_default))
);

create unique index sandbox_accounts_provider_name_active_idx
    on sandbox_accounts (provider, lower(name))
    where deleted_at is null;

create unique index sandbox_accounts_one_default_per_provider_idx
    on sandbox_accounts (provider)
    where is_default and deleted_at is null;

create index sandbox_accounts_enabled_provider_idx
    on sandbox_accounts (provider)
    where enabled and deleted_at is null;

create table sandbox_machines (
    machine_id text primary key references machines(machine_id) on delete restrict,
    sandbox_account_id uuid not null references sandbox_accounts(id) on delete restrict,
    provider_resource_id text not null,
    connection_secret_id uuid unique references execution_vault_secrets(id) on delete restrict,
    state text not null,
    spec jsonb not null default '{}'::jsonb,
    connection_config jsonb not null default '{}'::jsonb,
    provider_metadata jsonb not null default '{}'::jsonb,
    last_error jsonb,
    started_at timestamptz,
    expires_at timestamptz,
    stopped_at timestamptz,
    terminated_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint sandbox_machines_account_resource_unique
        unique (sandbox_account_id, provider_resource_id),
    constraint sandbox_machines_resource_not_blank
        check (provider_resource_id = btrim(provider_resource_id) and provider_resource_id <> ''),
    constraint sandbox_machines_state_valid
        check (state in ('provisioning', 'running', 'stopped', 'failed', 'terminating', 'terminated')),
    constraint sandbox_machines_spec_is_object check (jsonb_typeof(spec) = 'object'),
    constraint sandbox_machines_connection_config_is_object
        check (jsonb_typeof(connection_config) = 'object'),
    constraint sandbox_machines_provider_metadata_is_object
        check (jsonb_typeof(provider_metadata) = 'object'),
    constraint sandbox_machines_last_error_is_object
        check (last_error is null or jsonb_typeof(last_error) = 'object')
);

create index sandbox_machines_account_state_idx
    on sandbox_machines (sandbox_account_id, state);

create function execution_gateway_set_updated_at()
returns trigger
language plpgsql
as $$
begin
    new.updated_at = now();
    return new;
end;
$$;

create trigger execution_vault_secrets_set_updated_at
before update on execution_vault_secrets
for each row execute function execution_gateway_set_updated_at();

create trigger sandbox_accounts_set_updated_at
before update on sandbox_accounts
for each row execute function execution_gateway_set_updated_at();

create trigger sandbox_machines_set_updated_at
before update on sandbox_machines
for each row execute function execution_gateway_set_updated_at();
