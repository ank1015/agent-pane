-- Catalog registration is independent of worker capability advertisement.
insert into harnesses (id, name, description, default_config, config_schema, supported_models, enabled)
values (
  'unified-exec-only-harness',
  'Unified Exec Only Harness',
  'Durable shell-only coding loop using exec_command and write_stdin',
  '{"reasoning_level":"high"}'::jsonb,
  '{"additionalProperties":false,"properties":{"account_id":{"format":"uuid","type":["string","null"]},"environment":{"oneOf":[{"additionalProperties":false,"properties":{"machine_id":{"format":"uuid","type":"string"},"path":{"maxLength":4096,"minLength":1,"type":"string"},"type":{"const":"machine"},"workspace_root":{"description":"Absolute native workspace path returned by discovery; not a root ID.","maxLength":4096,"minLength":1,"pattern":"^(/|[A-Za-z]:[/\\\\]|\\\\\\\\)","type":"string"}},"required":["type","machine_id","workspace_root","path"],"type":"object"},{"additionalProperties":false,"properties":{"path":{"maxLength":4096,"minLength":1,"type":"string"},"snapshot_id":{"format":"uuid","type":"string"},"type":{"const":"sandbox"},"workspace_root":{"description":"Absolute native workspace path returned by discovery; not a root ID.","maxLength":4096,"minLength":1,"pattern":"^(/|[A-Za-z]:[/\\\\]|\\\\\\\\)","type":"string"}},"required":["type","snapshot_id","workspace_root","path"],"type":"object"}]},"model":{"additionalProperties":false,"properties":{"id":{"minLength":1,"type":"string"},"name":{"type":["string","null"]},"provider":{"enum":["openai","chatgpt","fireworks"]}},"required":["provider","id"],"type":"object"},"reasoning_level":{"enum":["low","medium","high","xhigh","max"]},"system_prompt_append":{"maxLength":8192,"type":["string","null"]}},"required":["model","reasoning_level","environment"],"type":"object"}'::jsonb,
  '{"chatgpt":["gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna"],"fireworks":["accounts/fireworks/models/glm-5p3-flash","accounts/fireworks/models/glm-5p3","accounts/fireworks/models/kimi-k3","accounts/fireworks/models/deepseek-v4-pro-0813","accounts/fireworks/models/deepseek-v4-flash-0731","accounts/fireworks/models/qwen3p8-2p4t-a95b"],"openai":["gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna"]}'::jsonb,
  true
)
on conflict (id) do nothing;
