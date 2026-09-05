create table provider_accounts (
    id uuid primary key,
    provider text not null,
    name text not null,
    config jsonb not null default '{}'::jsonb,
    status text not null default 'active',
    is_default boolean not null default false,
    runtime_revision bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,

    constraint provider_accounts_provider_not_blank
        check (provider = btrim(provider) and provider <> ''),
    constraint provider_accounts_name_not_blank
        check (name = btrim(name) and name <> ''),
    constraint provider_accounts_config_is_object
        check (jsonb_typeof(config) = 'object'),
    constraint provider_accounts_status_valid
        check (status in ('active', 'disabled', 'reauth_required')),
    constraint provider_accounts_runtime_revision_positive
        check (runtime_revision > 0),
    constraint provider_accounts_deleted_not_default
        check (deleted_at is null or not is_default)
);

create unique index provider_accounts_provider_name_active_idx
    on provider_accounts (provider, name)
    where deleted_at is null;

create unique index provider_accounts_one_default_per_provider_idx
    on provider_accounts (provider)
    where is_default and deleted_at is null;

create index provider_accounts_selectable_idx
    on provider_accounts (provider, status, is_default)
    where deleted_at is null;

create table provider_credentials (
    provider_account_id uuid primary key,
    encrypted_payload bytea not null,
    nonce bytea not null,
    encryption_key_version integer not null default 1,
    credential_version bigint not null default 1,
    expires_at timestamptz,
    refreshed_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint provider_credentials_account_fkey
        foreign key (provider_account_id) references provider_accounts (id) on delete cascade,
    constraint provider_credentials_encrypted_payload_not_empty
        check (octet_length(encrypted_payload) > 0),
    constraint provider_credentials_nonce_not_empty
        check (octet_length(nonce) > 0),
    constraint provider_credentials_encryption_key_version_positive
        check (encryption_key_version > 0),
    constraint provider_credentials_credential_version_positive
        check (credential_version > 0)
);

create table llm_requests (
    id uuid primary key,
    operation text not null,
    account_id uuid not null,
    provider text not null,
    requested_model text not null,
    response_provider text,
    response_model text,
    response_id text,
    status text not null default 'running',
    error_kind text,
    provider_error_code text,
    can_retry boolean,
    labels jsonb not null default '{}'::jsonb,
    provider_duration_ms bigint,
    gateway_duration_ms bigint,
    started_at timestamptz not null default now(),
    completed_at timestamptz,

    constraint llm_requests_account_fkey
        foreign key (account_id) references provider_accounts (id) on delete restrict,
    constraint llm_requests_operation_valid
        check (operation in ('complete', 'search')),
    constraint llm_requests_provider_not_blank
        check (provider = btrim(provider) and provider <> ''),
    constraint llm_requests_requested_model_not_blank
        check (requested_model = btrim(requested_model) and requested_model <> ''),
    constraint llm_requests_status_valid
        check (status in ('running', 'succeeded', 'provider_error', 'timeout', 'cancelled', 'internal_error')),
    constraint llm_requests_labels_is_object
        check (jsonb_typeof(labels) = 'object'),
    constraint llm_requests_provider_duration_non_negative
        check (provider_duration_ms is null or provider_duration_ms >= 0),
    constraint llm_requests_gateway_duration_non_negative
        check (gateway_duration_ms is null or gateway_duration_ms >= 0),
    constraint llm_requests_terminal_has_completed_at
        check ((status = 'running') = (completed_at is null))
);

create index llm_requests_completed_at_idx
    on llm_requests (completed_at desc, id desc);

create index llm_requests_account_idx
    on llm_requests (account_id, completed_at desc, id desc);

create index llm_requests_provider_model_idx
    on llm_requests (provider, requested_model, completed_at desc, id desc);

create index llm_requests_operation_status_idx
    on llm_requests (operation, status, completed_at desc, id desc);

create table llm_usage (
    request_id uuid primary key,
    usage jsonb not null,
    input_tokens bigint generated always as ((usage ->> 'input')::bigint) stored,
    output_tokens bigint generated always as ((usage ->> 'output')::bigint) stored,
    cache_read_tokens bigint generated always as ((usage ->> 'cache_read')::bigint) stored,
    cache_write_tokens bigint generated always as ((usage ->> 'cache_write')::bigint) stored,
    input_cost_usd numeric generated always as ((usage #>> '{cost,input}')::numeric) stored,
    output_cost_usd numeric generated always as ((usage #>> '{cost,output}')::numeric) stored,
    cache_read_cost_usd numeric generated always as ((usage #>> '{cost,cache_read}')::numeric) stored,
    cache_write_cost_usd numeric generated always as ((usage #>> '{cost,cache_write}')::numeric) stored,
    total_cost_usd numeric generated always as ((usage #>> '{cost,total}')::numeric) stored,
    created_at timestamptz not null default now(),

    constraint llm_usage_request_fkey
        foreign key (request_id) references llm_requests (id) on delete cascade,
    constraint llm_usage_usage_is_object
        check (jsonb_typeof(usage) = 'object')
);

create function set_updated_at()
returns trigger
language plpgsql
as $$
begin
    new.updated_at = now();
    return new;
end;
$$;

create trigger provider_accounts_set_updated_at
before update on provider_accounts
for each row execute function set_updated_at();

create trigger provider_credentials_set_updated_at
before update on provider_credentials
for each row execute function set_updated_at();
