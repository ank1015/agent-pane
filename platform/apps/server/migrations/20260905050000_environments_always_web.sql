-- Web search is an invariant of this harness, not a user configuration option.
update harnesses set
    default_config = default_config - 'web_search_enabled',
    config_schema = config_schema #- '{properties,web_search_enabled}',
    updated_at = clock_timestamp()
where id = 'environments';
