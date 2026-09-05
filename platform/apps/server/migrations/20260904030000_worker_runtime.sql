-- Credentials never appear in worker metadata or run context. Existing metadata
-- rows have no credentials and cannot authenticate until explicitly registered.
create table worker_credentials (
    worker_id uuid primary key references workers(id) on delete restrict,
    token_hash bytea not null check (octet_length(token_hash) = 32)
);
create table worker_claim_requests (
    worker_id uuid not null references workers(id) on delete restrict,
    key text not null check (char_length(key) between 1 and 256),
    request_hash text not null,
    result jsonb not null,
    created_at timestamptz not null default now(),
    primary key (worker_id, key)
);
create index run_waits_pending_run_idx on run_waits(run_id) where status = 'pending';
create index run_inputs_correlation_idx on run_inputs(run_id,kind,(payload->>'correlation_key'),sequence);
