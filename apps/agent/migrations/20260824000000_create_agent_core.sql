create table harnesses (
    harness_id text primary key,
    slug text not null unique,
    display_name text not null,
    description text,
    enabled boolean not null default false,
    active_revision_id text,
    created_at timestamptz not null default now(),

    constraint harnesses_id_valid check (
        harness_id = btrim(harness_id) and harness_id <> ''
    ),
    constraint harnesses_slug_valid check (
        octet_length(slug) between 1 and 64
        and slug ~ '^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$'
    ),
    constraint harnesses_display_name_valid check (
        display_name = btrim(display_name)
        and char_length(display_name) between 1 and 128
    ),
    constraint harnesses_description_valid check (
        description is null
        or (
            description = btrim(description)
            and char_length(description) between 1 and 2000
        )
    )
);

create table harness_revisions (
    harness_revision_id text primary key,
    harness_id text not null references harnesses (harness_id) on delete restrict,
    revision text not null,
    contract_version bigint not null,
    default_config jsonb not null default '{}'::jsonb,
    config_schema jsonb,
    first_activated_at timestamptz,
    retired_at timestamptz,
    created_at timestamptz not null default now(),

    constraint harness_revisions_id_harness_unique
        unique (harness_revision_id, harness_id),
    constraint harness_revisions_name_unique unique (harness_id, revision),
    constraint harness_revisions_id_valid check (
        harness_revision_id = btrim(harness_revision_id)
        and harness_revision_id <> ''
    ),
    constraint harness_revisions_revision_valid check (
        revision = btrim(revision)
        and char_length(revision) between 1 and 128
    ),
    constraint harness_revisions_contract_version_valid check (
        contract_version between 1 and 4294967295
    ),
    constraint harness_revisions_default_config_object check (
        jsonb_typeof(default_config) = 'object'
    ),
    constraint harness_revisions_config_schema_object check (
        config_schema is null or jsonb_typeof(config_schema) = 'object'
    ),
    constraint harness_revisions_activation_time_valid check (
        first_activated_at is null or first_activated_at >= created_at
    ),
    constraint harness_revisions_retirement_time_valid check (
        retired_at is null
        or retired_at >= coalesce(first_activated_at, created_at)
    )
);

alter table harnesses
    add constraint harnesses_active_revision_fkey
        foreign key (active_revision_id, harness_id)
        references harness_revisions (harness_revision_id, harness_id)
        on delete restrict;

create index harnesses_created_idx on harnesses (created_at, harness_id);
create index harness_revisions_harness_created_idx
    on harness_revisions (harness_id, created_at desc, harness_revision_id desc);

create table sessions (
    session_id uuid primary key,
    current_revision bigint not null default 0,
    created_at timestamptz not null default now(),

    constraint sessions_current_revision_valid check (current_revision >= 0)
);

create table session_messages (
    session_message_id uuid primary key,
    session_id uuid not null references sessions (session_id) on delete restrict,
    revision bigint,
    message jsonb not null,
    origin text not null,
    delivery text not null default 'immediate',
    state text not null,
    run_id uuid,
    turn_number integer,
    queued_during_turn integer,
    queue_sequence bigint,
    discard_reason text,
    created_at timestamptz not null default now(),
    committed_at timestamptz,
    discarded_at timestamptz,

    constraint session_messages_id_session_unique
        unique (session_message_id, session_id),
    constraint session_messages_revision_positive check (
        revision is null or revision > 0
    ),
    constraint session_messages_message_object check (
        jsonb_typeof(message) = 'object'
    ),
    constraint session_messages_origin_valid check (
        origin in ('external', 'harness', 'system', 'imported')
    ),
    constraint session_messages_delivery_valid check (
        delivery in ('immediate', 'next_turn')
    ),
    constraint session_messages_state_valid check (
        state in ('pending', 'committed', 'discarded')
    ),
    constraint session_messages_turn_positive check (
        turn_number is null or turn_number > 0
    ),
    constraint session_messages_queued_turn_positive check (
        queued_during_turn is null or queued_during_turn > 0
    ),
    constraint session_messages_queue_sequence_positive check (
        queue_sequence is null or queue_sequence > 0
    ),
    constraint session_messages_discard_reason_valid check (
        discard_reason is null
        or (
            discard_reason = btrim(discard_reason)
            and char_length(discard_reason) between 1 and 128
        )
    ),
    constraint session_messages_state_consistent check (
        (
            state = 'pending'
            and revision is null
            and turn_number is null
            and committed_at is null
            and discarded_at is null
            and discard_reason is null
        )
        or (
            state = 'committed'
            and revision is not null
            and committed_at is not null
            and discarded_at is null
            and discard_reason is null
        )
        or (
            state = 'discarded'
            and revision is null
            and turn_number is null
            and committed_at is null
            and discarded_at is not null
            and discard_reason is not null
        )
    ),
    constraint session_messages_delivery_consistent check (
        (
            delivery = 'immediate'
            and state = 'committed'
            and queued_during_turn is null
            and queue_sequence is null
        )
        or (
            delivery = 'next_turn'
            and run_id is not null
            and queued_during_turn is not null
            and queue_sequence is not null
            and origin in ('external', 'system')
        )
    ),
    constraint session_messages_harness_lineage check (
        origin <> 'harness' or (run_id is not null and turn_number is not null)
    ),
    constraint session_messages_commit_time_valid check (
        committed_at is null or committed_at >= created_at
    ),
    constraint session_messages_discard_time_valid check (
        discarded_at is null or discarded_at >= created_at
    )
);

create unique index session_messages_revision_idx
    on session_messages (session_id, revision)
    where revision is not null;

create unique index session_messages_run_queue_idx
    on session_messages (run_id, queue_sequence)
    where delivery = 'next_turn';

create index session_messages_pending_idx
    on session_messages (run_id, queue_sequence)
    where state = 'pending';

create index session_messages_run_turn_idx
    on session_messages (run_id, turn_number, revision)
    where run_id is not null and state = 'committed';

create table runs (
    run_id uuid primary key,
    session_id uuid not null references sessions (session_id) on delete restrict,
    trigger_message_id uuid not null,
    harness_revision_id text not null
        references harness_revisions (harness_revision_id) on delete restrict,
    resolved_config jsonb not null default '{}'::jsonb,
    status text not null default 'queued',
    current_turn integer not null default 1,
    max_turns integer not null,
    failures_in_current_turn integer not null default 0,
    max_failures_per_turn integer not null,
    state_version bigint not null default 1,
    queued_at timestamptz default now(),
    final_message_id uuid,
    failure jsonb,
    created_at timestamptz not null default now(),
    started_at timestamptz,
    finished_at timestamptz,

    constraint runs_run_session_unique unique (run_id, session_id),
    constraint runs_trigger_message_unique unique (trigger_message_id),
    constraint runs_trigger_message_fkey
        foreign key (trigger_message_id, session_id)
        references session_messages (session_message_id, session_id)
        on delete restrict,
    constraint runs_final_message_fkey
        foreign key (final_message_id, session_id)
        references session_messages (session_message_id, session_id)
        on delete restrict,
    constraint runs_resolved_config_object check (
        jsonb_typeof(resolved_config) = 'object'
    ),
    constraint runs_status_valid check (
        status in (
            'queued', 'running', 'waiting', 'aborting',
            'aborted', 'completed', 'failed'
        )
    ),
    constraint runs_turn_limits_valid check (
        max_turns > 0 and current_turn between 1 and max_turns
    ),
    constraint runs_failure_limits_valid check (
        max_failures_per_turn > 0
        and failures_in_current_turn between 0 and max_failures_per_turn
    ),
    constraint runs_state_version_positive check (state_version > 0),
    constraint runs_failure_object check (
        failure is null or jsonb_typeof(failure) = 'object'
    ),
    constraint runs_queue_state_consistent check (
        (status = 'queued') = (queued_at is not null)
    ),
    constraint runs_terminal_state_consistent check (
        (status in ('aborted', 'completed', 'failed')) = (finished_at is not null)
    ),
    constraint runs_final_message_consistent check (
        (status = 'completed') = (final_message_id is not null)
    ),
    constraint runs_failure_consistent check (
        (status = 'failed') = (failure is not null)
    ),
    constraint runs_timestamps_valid check (
        (started_at is null or started_at >= created_at)
        and (
            finished_at is null
            or finished_at >= coalesce(started_at, created_at)
        )
    )
);

create unique index runs_one_active_per_session_idx
    on runs (session_id)
    where status in ('queued', 'running', 'waiting', 'aborting');

create index runs_queue_idx
    on runs (queued_at, run_id)
    where status = 'queued';

create index runs_harness_revision_created_idx
    on runs (harness_revision_id, created_at, run_id);

alter table session_messages
    add constraint session_messages_run_session_fkey
        foreign key (run_id, session_id)
        references runs (run_id, session_id)
        on delete restrict;

create table run_leases (
    lease_id uuid primary key,
    run_id uuid not null unique references runs (run_id) on delete restrict,
    lease_version bigint not null,
    worker_instance_id text not null,
    token_hash bytea not null,
    acquired_at timestamptz not null default now(),
    expires_at timestamptz not null,

    constraint run_leases_version_positive check (lease_version > 0),
    constraint run_leases_worker_valid check (
        worker_instance_id = btrim(worker_instance_id)
        and worker_instance_id <> ''
    ),
    constraint run_leases_token_hash_valid check (octet_length(token_hash) = 32),
    constraint run_leases_expiry_valid check (expires_at > acquired_at)
);

create index run_leases_expiry_idx on run_leases (expires_at, lease_id);

create table run_waits (
    wait_id uuid primary key,
    run_id uuid not null references runs (run_id) on delete restrict,
    turn_number integer not null,
    harness_wait_id text not null,
    kind text not null,
    public_request jsonb not null,
    resume_metadata jsonb not null default '{}'::jsonb,
    status text not null default 'pending',
    resolution jsonb,
    requested_at timestamptz not null default now(),
    resolved_at timestamptz,
    cancelled_at timestamptz,

    constraint run_waits_harness_id_unique unique (run_id, harness_wait_id),
    constraint run_waits_turn_positive check (turn_number > 0),
    constraint run_waits_harness_wait_id_valid check (
        harness_wait_id = btrim(harness_wait_id)
        and char_length(harness_wait_id) between 1 and 256
    ),
    constraint run_waits_kind_valid check (
        kind = btrim(kind) and char_length(kind) between 1 and 128
    ),
    constraint run_waits_public_request_object check (
        jsonb_typeof(public_request) = 'object'
    ),
    constraint run_waits_resume_metadata_object check (
        jsonb_typeof(resume_metadata) = 'object'
    ),
    constraint run_waits_resolution_object check (
        resolution is null or jsonb_typeof(resolution) = 'object'
    ),
    constraint run_waits_status_valid check (
        status in ('pending', 'resolved', 'cancelled')
    ),
    constraint run_waits_state_consistent check (
        (
            status = 'pending'
            and resolution is null
            and resolved_at is null
            and cancelled_at is null
        )
        or (
            status = 'resolved'
            and resolution is not null
            and resolved_at is not null
            and cancelled_at is null
        )
        or (
            status = 'cancelled'
            and resolution is null
            and resolved_at is null
            and cancelled_at is not null
        )
    ),
    constraint run_waits_timestamps_valid check (
        (resolved_at is null or resolved_at >= requested_at)
        and (cancelled_at is null or cancelled_at >= requested_at)
    )
);

create unique index run_waits_one_pending_per_run_idx
    on run_waits (run_id)
    where status = 'pending';

create index run_waits_run_requested_idx
    on run_waits (run_id, requested_at, wait_id);

create index run_waits_pending_requested_idx
    on run_waits (requested_at, wait_id)
    where status = 'pending';

create table run_aborts (
    abort_id uuid primary key,
    run_id uuid not null references runs (run_id) on delete restrict,
    turn_number integer not null,
    sequence bigint not null,
    reason text,
    payload jsonb not null default '{}'::jsonb,
    status text not null default 'pending',
    requested_at timestamptz not null default now(),
    delivered_at timestamptz,
    deadline_at timestamptz not null,
    finalized_at timestamptz,
    finalization_reason text,
    resume_metadata jsonb not null default '{}'::jsonb,
    resolution jsonb,
    resumed_at timestamptz,
    resumed_turn_number integer,

    constraint run_aborts_sequence_unique unique (run_id, sequence),
    constraint run_aborts_turn_positive check (turn_number > 0),
    constraint run_aborts_sequence_positive check (sequence > 0),
    constraint run_aborts_reason_valid check (
        reason is null
        or (
            reason = btrim(reason)
            and char_length(reason) between 1 and 2000
        )
    ),
    constraint run_aborts_payload_object check (jsonb_typeof(payload) = 'object'),
    constraint run_aborts_resume_metadata_object check (
        jsonb_typeof(resume_metadata) = 'object'
    ),
    constraint run_aborts_resolution_object check (
        resolution is null or jsonb_typeof(resolution) = 'object'
    ),
    constraint run_aborts_status_valid check (
        status in ('pending', 'finalized', 'resumed')
    ),
    constraint run_aborts_finalization_reason_valid check (
        finalization_reason is null
        or finalization_reason in (
            'acknowledged', 'no_worker', 'lease_expired', 'deadline_expired'
        )
    ),
    constraint run_aborts_resumed_turn_positive check (
        resumed_turn_number is null or resumed_turn_number > 0
    ),
    constraint run_aborts_state_consistent check (
        (
            status = 'pending'
            and finalized_at is null
            and finalization_reason is null
            and resume_metadata = '{}'::jsonb
            and resolution is null
            and resumed_at is null
            and resumed_turn_number is null
        )
        or (
            status = 'finalized'
            and finalized_at is not null
            and finalization_reason is not null
            and resolution is null
            and resumed_at is null
            and resumed_turn_number is null
        )
        or (
            status = 'resumed'
            and finalized_at is not null
            and finalization_reason is not null
            and resolution is not null
            and resumed_at is not null
            and resumed_turn_number is not null
        )
    ),
    constraint run_aborts_timestamps_valid check (
        deadline_at >= requested_at
        and (
            delivered_at is null
            or delivered_at between requested_at and deadline_at
        )
        and (finalized_at is null or finalized_at >= requested_at)
        and (resumed_at is null or resumed_at >= finalized_at)
    )
);

create unique index run_aborts_one_pending_per_run_idx
    on run_aborts (run_id)
    where status = 'pending';

create index run_aborts_run_sequence_idx
    on run_aborts (run_id, sequence);

create index run_aborts_pending_deadline_idx
    on run_aborts (deadline_at, abort_id)
    where status = 'pending';

comment on table session_messages is
    'Canonical transcript messages and next-turn steering messages in one admission lifecycle.';

comment on column session_messages.revision is
    'Assigned only when a message enters the canonical linear transcript.';

comment on table run_leases is
    'Current worker ownership only; rows are replaced or deleted rather than retained as attempt history.';

comment on column runs.state_version is
    'Monotonic optimistic-concurrency version for durable run transitions.';
