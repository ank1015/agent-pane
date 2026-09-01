alter table project_harness_runs
    add column idempotency_key text,
    add column request_hash bytea,
    add column creation_state text not null default 'pending',
    add column accepted_at timestamptz;

update project_harness_runs run
set idempotency_key = session.idempotency_key,
    request_hash = session.request_hash,
    creation_state = session.creation_state,
    accepted_at = session.accepted_at
from project_harness_sessions session
where session.session_id = run.session_id;

alter table project_harness_runs
    alter column idempotency_key set not null,
    alter column request_hash set not null,
    add constraint project_harness_runs_idempotency_key_valid check (
        idempotency_key = btrim(idempotency_key)
        and char_length(idempotency_key) between 1 and 255
    ),
    add constraint project_harness_runs_request_hash_valid check (
        octet_length(request_hash) = 32
    ),
    add constraint project_harness_runs_creation_state_valid check (
        creation_state in ('pending', 'accepted')
    ),
    add constraint project_harness_runs_accepted_at_valid check (
        (creation_state = 'pending' and accepted_at is null)
        or (creation_state = 'accepted' and accepted_at is not null)
    ),
    add constraint project_harness_runs_session_idempotency_key_unique unique (
        session_id,
        idempotency_key
    );
