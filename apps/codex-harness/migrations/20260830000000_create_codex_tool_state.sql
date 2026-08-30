create table codex_exec_sessions (
    public_session_id integer generated always as identity primary key,
    agent_session_id uuid not null,
    execution_id text not null,
    machine_id text not null,
    tty boolean not null,
    last_sequence bigint not null default 0,
    created_at timestamptz not null default now(),
    last_accessed_at timestamptz not null default now(),

    constraint codex_exec_sessions_execution_id_not_blank
        check (btrim(execution_id) <> ''),
    constraint codex_exec_sessions_machine_id_not_blank
        check (btrim(machine_id) <> ''),
    constraint codex_exec_sessions_last_sequence_nonnegative
        check (last_sequence >= 0),
    constraint codex_exec_sessions_agent_execution_unique
        unique (agent_session_id, execution_id)
);

create index codex_exec_sessions_agent_session_idx
    on codex_exec_sessions(agent_session_id, public_session_id);

create table codex_code_mode_state (
    agent_session_id uuid primary key,
    next_cell_id bigint not null default 1,
    stored_values jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),

    constraint codex_code_mode_state_next_cell_id_positive
        check (next_cell_id > 0),
    constraint codex_code_mode_state_values_object
        check (jsonb_typeof(stored_values) = 'object')
);
