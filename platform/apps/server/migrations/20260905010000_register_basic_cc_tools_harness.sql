-- Catalog availability is independent of a worker build's advertised capability.
-- Preserve any deliberately preconfigured row with this ID.
insert into harnesses(id, name, description, default_config, config_schema, enabled)
values (
  'basic-cc-tools-harness',
  'Basic CC Tools Harness',
  'Sequential read/write/edit/bash coding loop without compaction',
  '{"reasoning_level":"high"}'::jsonb,
  '{
    "type":"object", "additionalProperties":false,
    "required":["model","reasoning_level","execution"],
    "properties":{
      "model":{"type":"object","additionalProperties":false,"required":["provider","id"],"properties":{
        "provider":{"enum":["openai","chatgpt","fireworks"]},
        "id":{"type":"string","minLength":1},"name":{"type":["string","null"]}}},
      "reasoning_level":{"enum":["low","medium","high","xhigh","max"]},
      "account_id":{"type":["string","null"],"format":"uuid"},
      "execution":{"type":"object","additionalProperties":false,"required":["host_id","workspace_root","path"],"properties":{
        "host_id":{"type":"string","format":"uuid"},"workspace_root":{"type":"string","minLength":1},
        "path":{"type":"string","minLength":1,"maxLength":4096}}},
      "system_prompt_append":{"type":["string","null"],"maxLength":8192}
    }
  }'::jsonb,
  true
)
on conflict (id) do nothing;
