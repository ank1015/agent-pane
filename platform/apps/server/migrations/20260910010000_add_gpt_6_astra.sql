-- Keep every OpenAI/ChatGPT-capable harness aligned with the provider catalogs.
-- Replacing only these provider keys preserves each harness's other providers.
update harnesses
set supported_models = supported_models || '{"openai":["gpt-6-astra","gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna"],"chatgpt":["gpt-6-astra","gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna"]}'::jsonb,
    updated_at = clock_timestamp()
where id in (
    'environments',
    'basic-cc-tools-harness',
    'basic-codex-tools-harness',
    'unified-exec-only-harness',
    'sites'
);
