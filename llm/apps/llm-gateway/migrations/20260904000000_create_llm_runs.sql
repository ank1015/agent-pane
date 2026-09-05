create table llm_runs (
    id uuid primary key,
    idempotency_key text not null unique,
    request_hash bytea not null,
    status text not null default 'running'
        check (status in ('running', 'succeeded', 'failed', 'expired')),
    result jsonb,
    created_at timestamptz not null default now(),
    completed_at timestamptz,
    expires_at timestamptz,
    check ((status = 'running') = (completed_at is null))
);
create index llm_runs_expiry_idx on llm_runs (expires_at);
