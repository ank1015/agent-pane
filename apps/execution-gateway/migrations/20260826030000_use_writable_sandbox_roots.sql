update machines m
set descriptor = jsonb_set(
        jsonb_set(
            m.descriptor,
            '{workspace_roots,0,name}',
            to_jsonb('Sandbox workspace'::text)
        ),
        '{workspace_roots,0,uri}',
        to_jsonb(
            case a.provider
                when 'e2b' then 'file:///home/user'
                when 'daytona' then 'file:///home/daytona'
                when 'blaxel' then 'file:///blaxel'
                when 'tensorlake' then 'file:///home/tl-user'
            end
        )
    ),
    updated_at = now()
from sandbox_machines sm
join sandbox_accounts a on a.id = sm.sandbox_account_id
where m.machine_id = sm.machine_id
  and m.deleted_at is null;
