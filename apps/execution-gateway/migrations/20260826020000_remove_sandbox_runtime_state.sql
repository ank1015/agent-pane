drop index sandbox_machines_account_state_idx;

alter table sandbox_machines
    drop column state,
    drop column started_at,
    drop column expires_at,
    drop column stopped_at,
    drop column terminated_at;
