alter table harnesses
    add column supported_providers jsonb not null default '[]'::jsonb,
    add constraint harnesses_supported_providers_array
        check (jsonb_typeof(supported_providers) = 'array');
