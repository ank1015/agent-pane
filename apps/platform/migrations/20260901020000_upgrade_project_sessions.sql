alter table project_harness_sessions rename to project_sessions;
alter table project_sessions rename column session_id to id;

alter table project_harness_runs rename to project_session_runs;

alter table project_sessions
    rename constraint project_harness_sessions_pkey to project_sessions_pkey;
alter table project_session_runs
    rename constraint project_harness_runs_pkey to project_session_runs_pkey;

alter table project_sessions
    rename constraint project_harness_sessions_project_id_fkey
        to project_sessions_project_id_fkey;
alter table project_sessions
    rename constraint project_harness_sessions_harness_id_valid
        to project_sessions_harness_id_valid;
alter table project_sessions
    rename constraint project_harness_sessions_idempotency_key_valid
        to project_sessions_idempotency_key_valid;
alter table project_sessions
    rename constraint project_harness_sessions_request_hash_valid
        to project_sessions_request_hash_valid;
alter table project_sessions
    rename constraint project_harness_sessions_project_id_idempotency_key_key
        to project_sessions_project_id_idempotency_key_unique;

alter table project_session_runs
    rename constraint project_harness_runs_session_id_fkey
        to project_session_runs_session_id_fkey;
alter table project_session_runs
    rename constraint project_harness_runs_trigger_message_id_key
        to project_session_runs_trigger_message_id_unique;
alter table project_session_runs
    rename constraint project_harness_runs_sequence_valid
        to project_session_runs_sequence_valid;
alter table project_session_runs
    rename constraint project_harness_runs_session_id_run_sequence_key
        to project_session_runs_session_sequence_unique;
alter table project_session_runs
    rename constraint project_harness_runs_idempotency_key_valid
        to project_session_runs_idempotency_key_valid;
alter table project_session_runs
    rename constraint project_harness_runs_request_hash_valid
        to project_session_runs_request_hash_valid;
alter table project_session_runs
    rename constraint project_harness_runs_session_idempotency_key_unique
        to project_session_runs_session_idempotency_key_unique;

alter table project_sessions
    drop constraint project_harness_sessions_creation_state_valid,
    drop constraint project_harness_sessions_accepted_at_valid,
    add column title text,
    add column harness_revision_id text,
    add column harness_config jsonb not null default '{}'::jsonb,
    add column web_search_enabled boolean not null default true,
    add column active_run_id uuid,
    add column current_revision bigint not null default 0,
    add column creation_error jsonb,
    add column last_activity_at timestamptz,
    add column activity_checked_at timestamptz,
    add column archived_at timestamptz;

update project_sessions
set title = harness_id,
    last_activity_at = coalesce(accepted_at, created_at),
    creation_state = 'failed',
    accepted_at = null,
    creation_error = jsonb_build_object(
        'code', 'legacy_session_requires_recreation',
        'message', 'Session predates immutable harness configuration snapshots'
    );

alter table project_sessions
    alter column title set not null,
    alter column last_activity_at set not null,
    add constraint project_sessions_title_valid check (
        title = btrim(title) and char_length(title) between 1 and 255
    ),
    add constraint project_sessions_harness_revision_id_valid check (
        harness_revision_id is null
        or (
            harness_revision_id = btrim(harness_revision_id)
            and char_length(harness_revision_id) between 1 and 255
        )
    ),
    add constraint project_sessions_current_revision_valid check (current_revision >= 0),
    add constraint project_sessions_creation_state_valid check (
        creation_state in ('pending', 'accepted', 'failed')
    ),
    add constraint project_sessions_creation_state_fields_valid check (
        (
            creation_state = 'pending'
            and accepted_at is null
            and creation_error is null
        )
        or (
            creation_state = 'accepted'
            and accepted_at is not null
            and creation_error is null
            and harness_revision_id is not null
        )
        or (
            creation_state = 'failed'
            and accepted_at is null
            and creation_error is not null
        )
    );

alter table project_session_runs
    drop constraint project_harness_runs_creation_state_valid,
    drop constraint project_harness_runs_accepted_at_valid,
    add column status text,
    add column current_turn integer,
    add column max_turns integer,
    add column state_version bigint,
    add column failure jsonb,
    add column activated_at timestamptz,
    add column finished_at timestamptz,
    add column creation_error jsonb;

update project_session_runs
set creation_state = 'failed',
    accepted_at = null,
    creation_error = jsonb_build_object(
        'code', 'legacy_run_requires_recreation',
        'message', 'Run predates durable Platform run state'
    );

alter table project_session_runs
    add constraint project_session_runs_status_valid check (
        status is null
        or status in ('active', 'waiting', 'aborted', 'completed', 'failed')
    ),
    add constraint project_session_runs_current_turn_valid check (
        current_turn is null or current_turn >= 0
    ),
    add constraint project_session_runs_max_turns_valid check (
        max_turns is null or max_turns > 0
    ),
    add constraint project_session_runs_state_version_valid check (
        state_version is null or state_version >= 0
    ),
    add constraint project_session_runs_creation_state_valid check (
        creation_state in ('pending', 'accepted', 'failed')
    ),
    add constraint project_session_runs_creation_state_fields_valid check (
        (
            creation_state = 'pending'
            and accepted_at is null
            and creation_error is null
        )
        or (
            creation_state = 'accepted'
            and accepted_at is not null
            and creation_error is null
            and status is not null
        )
        or (
            creation_state = 'failed'
            and accepted_at is null
            and creation_error is not null
        )
    );

alter table project_sessions
    add constraint project_sessions_active_run_id_fkey
    foreign key (active_run_id)
    references project_session_runs (run_id)
    on delete set null;

create table project_session_environment_bindings (
    session_id uuid not null references project_sessions (id) on delete cascade,
    position integer not null,
    environment_id text not null,
    environment_snapshot jsonb not null,
    constraint project_session_environment_bindings_pkey primary key (session_id, position),
    constraint project_session_environment_bindings_position_valid check (position >= 0),
    constraint project_session_environment_bindings_environment_id_valid check (
        environment_id = btrim(environment_id)
        and char_length(environment_id) between 1 and 255
    ),
    constraint project_session_environment_bindings_session_environment_unique
        unique (session_id, environment_id)
);

alter index project_harness_sessions_project_created_idx
    rename to project_sessions_project_created_idx;
alter index project_harness_runs_session_created_idx
    rename to project_session_runs_session_created_idx;

create index project_sessions_project_activity_idx
    on project_sessions (project_id, last_activity_at desc, id desc)
    where archived_at is null;

create index project_sessions_active_run_idx
    on project_sessions (active_run_id)
    where active_run_id is not null;

create index project_sessions_active_reconcile_idx
    on project_sessions (activity_checked_at asc nulls first, active_run_id)
    where active_run_id is not null;

create index project_session_environment_bindings_environment_id_idx
    on project_session_environment_bindings (environment_id);
