-- Registration describes a harness-specific environment, not a Platform launch policy.
update harnesses set
    config_schema = jsonb_set(jsonb_set(config_schema #- '{properties,execution}',
        '{properties,environment}', '{"oneOf":[{"type":"object","additionalProperties":false,"required":["type","machine_id","workspace_root","path"],"properties":{"type":{"const":"machine"},"machine_id":{"type":"string","format":"uuid"},"workspace_root":{"type":"string","minLength":1,"maxLength":128},"path":{"type":"string","minLength":1,"maxLength":4096}}},{"type":"object","additionalProperties":false,"required":["type","snapshot_id","workspace_root","path"],"properties":{"type":{"const":"sandbox"},"snapshot_id":{"type":"string","format":"uuid"},"workspace_root":{"type":"string","minLength":1,"maxLength":128},"path":{"type":"string","minLength":1,"maxLength":4096}}}]}'::jsonb),
        '{required}', '["model","reasoning_level","environment"]'::jsonb),
    default_config = default_config - 'execution',
    updated_at = clock_timestamp()
where id='basic-cc-tools-harness';
