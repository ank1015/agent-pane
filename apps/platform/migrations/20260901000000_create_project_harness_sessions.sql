create table project_harness_sessions (
    session_id uuid primary key,
    project_id uuid not null references projects (project_id) on delete cascade,
    harness_id text not null,
    idempotency_key text not null,
    request_hash bytea not null,
    creation_state text not null default 'pending',
    created_at timestamptz not null default now(),
    accepted_at timestamptz,
    constraint project_harness_sessions_harness_id_valid check (
        harness_id = btrim(harness_id) and char_length(harness_id) between 1 and 255
    ),
    constraint project_harness_sessions_idempotency_key_valid check (
        idempotency_key = btrim(idempotency_key)
        and char_length(idempotency_key) between 1 and 255
    ),
    constraint project_harness_sessions_request_hash_valid check (
        octet_length(request_hash) = 32
    ),
    constraint project_harness_sessions_creation_state_valid check (
        creation_state in ('pending', 'accepted')
    ),
    constraint project_harness_sessions_accepted_at_valid check (
        (creation_state = 'pending' and accepted_at is null)
        or (creation_state = 'accepted' and accepted_at is not null)
    ),
    unique (project_id, idempotency_key)
);

create table project_harness_runs (
    run_id uuid primary key,
    session_id uuid not null references project_harness_sessions (session_id) on delete cascade,
    trigger_message_id uuid not null unique,
    run_sequence integer not null,
    created_at timestamptz not null default now(),
    constraint project_harness_runs_sequence_valid check (run_sequence > 0),
    unique (session_id, run_sequence)
);

create index project_harness_sessions_project_created_idx
    on project_harness_sessions (project_id, created_at, session_id);

create index project_harness_runs_session_created_idx
    on project_harness_runs (session_id, created_at, run_id);
