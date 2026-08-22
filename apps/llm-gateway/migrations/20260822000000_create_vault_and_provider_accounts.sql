create table vault_secrets (
    id uuid primary key,
    encrypted_payload bytea not null,
    nonce bytea not null,
    encryption_key_version integer not null default 1,
    credential_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint vault_secrets_encrypted_payload_not_empty
        check (octet_length(encrypted_payload) > 0),
    constraint vault_secrets_nonce_not_empty
        check (octet_length(nonce) > 0),
    constraint vault_secrets_encryption_key_version_positive
        check (encryption_key_version > 0),
    constraint vault_secrets_credential_version_positive
        check (credential_version > 0)
);

create table provider_accounts (
    id uuid primary key,
    provider text not null,
    name text not null,
    secret_id uuid not null,
    config jsonb not null default '{}'::jsonb,
    enabled boolean not null default true,
    is_default boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint provider_accounts_secret_id_unique unique (secret_id),
    constraint provider_accounts_provider_name_unique unique (provider, name),
    constraint provider_accounts_secret_id_fkey
        foreign key (secret_id) references vault_secrets (id) on delete restrict,
    constraint provider_accounts_provider_not_blank
        check (provider = btrim(provider) and provider <> ''),
    constraint provider_accounts_name_not_blank
        check (name = btrim(name) and name <> ''),
    constraint provider_accounts_config_is_object
        check (jsonb_typeof(config) = 'object'),
    constraint provider_accounts_default_is_enabled
        check (not is_default or enabled)
);

create unique index provider_accounts_one_default_per_provider_idx
    on provider_accounts (provider)
    where is_default;

create index provider_accounts_enabled_by_provider_idx
    on provider_accounts (provider)
    where enabled;

create function set_updated_at()
returns trigger
language plpgsql
as $$
begin
    new.updated_at = now();
    return new;
end;
$$;

create trigger vault_secrets_set_updated_at
before update on vault_secrets
for each row execute function set_updated_at();

create trigger provider_accounts_set_updated_at
before update on provider_accounts
for each row execute function set_updated_at();
