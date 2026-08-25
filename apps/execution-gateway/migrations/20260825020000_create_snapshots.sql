create table snapshots (
    id uuid primary key,
    provider text not null,
    provider_snapshot_id text not null,
    sandbox_id text not null,
    created_at timestamptz not null default now(),

    constraint snapshots_provider_supported
        check (provider in ('e2b', 'daytona', 'blaxel', 'tensorlake')),
    constraint snapshots_provider_snapshot_id_not_blank
        check (provider_snapshot_id = btrim(provider_snapshot_id) and provider_snapshot_id <> ''),
    constraint snapshots_sandbox_id_not_blank
        check (sandbox_id = btrim(sandbox_id) and sandbox_id <> ''),
    constraint snapshots_provider_snapshot_id_unique
        unique (provider, provider_snapshot_id)
);

create index snapshots_provider_sandbox_created_idx
    on snapshots(provider, sandbox_id, created_at desc, id);
