-- E2B sandbox timeouts control how long a sandbox runs before auto-pausing;
-- they are not resource expiration deadlines. Sandboxes persist until an
-- explicit termination request.
alter table platform_remote_operations
    rename column expires_at to deadline_at;

alter table platform_remote_operations
    alter column deadline_at drop not null;

update platform_remote_operations
set deadline_at = null,
    status = case when status = 'expired' then 'terminated' else status end
where kind = 'sandbox';

alter table platform_remote_operations
    drop constraint platform_remote_operations_status_check,
    add constraint platform_remote_operations_status_check check (
        status in ('provisioning','ready','unavailable','terminating','terminated','pending','running','completed','failed','cancelled','lost')
    ),
    add constraint platform_remote_operations_deadline_kind check (
        (kind = 'sandbox' and deadline_at is null)
        or (kind = 'execution' and deadline_at is not null)
    );

-- Ready sandboxes need no periodic reconciliation. Explicit termination sets
-- next_attempt_at back to the current time.
update platform_remote_operations
set next_attempt_at = 'infinity'::timestamptz
where kind = 'sandbox'
  and status = 'ready'
  and finished_at is null
  and not cancel_requested;
