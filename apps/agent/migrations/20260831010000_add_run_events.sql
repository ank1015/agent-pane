alter table runs
    add column last_event_sequence bigint not null default 0,
    add constraint runs_last_event_sequence_valid check (last_event_sequence >= 0);

create table run_events (
    run_id uuid not null references runs (run_id) on delete restrict,
    sequence bigint not null,
    event_id uuid not null unique,
    turn_number integer,
    state_version bigint not null,
    run_status text not null,
    source text not null,
    payload jsonb not null,
    occurred_at timestamptz not null,
    recorded_at timestamptz not null default now(),
    primary key (run_id, sequence),
    constraint run_events_sequence_positive check (sequence > 0),
    constraint run_events_turn_positive check (turn_number is null or turn_number > 0),
    constraint run_events_state_version_positive check (state_version > 0),
    constraint run_events_run_status_valid check (
        run_status in ('active', 'waiting', 'aborted', 'completed', 'failed')
    ),
    constraint run_events_source_valid check (source in ('agent', 'harness')),
    constraint run_events_payload_object check (jsonb_typeof(payload) = 'object')
);

create index run_events_recorded_idx on run_events (recorded_at, event_id);

comment on table run_events is
    'Canonical replayable event history for Agent run lifecycle and harness progress observations.';
