alter table harnesses
    add column supported_reasoning_levels jsonb not null default '[]'::jsonb,
    add constraint harnesses_supported_reasoning_levels_array
        check (jsonb_typeof(supported_reasoning_levels) = 'array');
