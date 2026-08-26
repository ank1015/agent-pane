update machines m
set name = template.name || ' ' || left(sandbox.provider_resource_id, 6),
    updated_at = now()
from sandbox_machines sandbox,
     environments environment,
     sandbox_environment_instances instance,
     sandbox_environment_templates template
where sandbox.machine_id = m.machine_id
  and environment.machine_id = m.machine_id
  and instance.environment_id = environment.environment_id
  and template.id = instance.template_id
  and m.deleted_at is null
  and environment.deleted_at is null
  and template.deleted_at is null
  and m.name = template.name || ' sandbox';
