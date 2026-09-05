-- Basic CC uses the same compiled provider policy as environments.
update harnesses set supported_models = '{"openai":["gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna"],"chatgpt":["gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna"],"fireworks":["accounts/fireworks/models/glm-5p3-flash","accounts/fireworks/models/glm-5p3","accounts/fireworks/models/kimi-k3","accounts/fireworks/models/deepseek-v4-pro-0813","accounts/fireworks/models/deepseek-v4-flash-0731","accounts/fireworks/models/qwen3p8-2p4t-a95b"]}'::jsonb,
    updated_at = clock_timestamp()
where id = 'basic-cc-tools-harness';
