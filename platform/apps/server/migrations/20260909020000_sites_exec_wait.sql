-- Raw exec uses a custom grammar tool. The Fireworks adapter rejects these;
-- leave other harnesses and their provider capabilities unchanged.
update harnesses
set supported_models = supported_models - 'fireworks',
    config_schema = jsonb_set(config_schema,
        '{properties,model,properties,provider,enum}', '["openai","chatgpt"]'::jsonb),
    updated_at = clock_timestamp()
where id = 'sites';
