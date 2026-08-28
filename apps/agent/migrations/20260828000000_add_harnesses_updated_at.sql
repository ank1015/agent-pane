alter table harnesses
    add column updated_at timestamptz not null default now();

create function agent_set_updated_at()
returns trigger
language plpgsql
as $$
begin
    new.updated_at = now();
    return new;
end;
$$;

create trigger harnesses_set_updated_at
before update on harnesses
for each row execute function agent_set_updated_at();
