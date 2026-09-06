#!/usr/bin/env python3
"""Install the harness-browser example into an already authorized project."""
import argparse, base64, hashlib, json, os, pathlib, urllib.parse, urllib.request, uuid
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--project', required=True, type=uuid.UUID)
p.add_argument('--platform', default='http://127.0.0.1:3100')
p.add_argument('--sites-service', default='http://127.0.0.1:3102')
p.add_argument('--dashboard', default='http://127.0.0.1:5173')
a = p.parse_args()
for name in ('platform', 'sites_service', 'dashboard'):
    value = getattr(a, name).rstrip('/')
    origin = urllib.parse.urlsplit(value)
    if (origin.username or origin.password or origin.path or origin.query or origin.fragment
            or not origin.hostname or not (origin.scheme == 'https'
            or (origin.scheme == 'http' and origin.hostname in ('127.0.0.1', 'localhost', '::1')))):
        p.error(f'--{name.replace("_", "-")} must be an HTTPS origin or loopback HTTP origin.')
    setattr(a, name, value)
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None
opener = urllib.request.build_opener(NoRedirect)
project_token = os.environ.get('SITE_PROJECT_TOKEN')
service_token = os.environ.get('SITES_API_TOKEN')
if not project_token or not service_token:
    p.error('Set SITE_PROJECT_TOKEN and SITES_API_TOKEN in the process environment.')
def call(base, token, path, method='GET', body=None, headers=None):
    request = urllib.request.Request(base+path, data=None if body is None else json.dumps(body).encode(), method=method,
        headers={'Authorization': 'Bearer '+token, 'Content-Type':'application/json', **(headers or {})})
    with opener.open(request, timeout=45) as r:
        return json.load(r)
def file(path, data):
    return {'path':path,'content_base64':base64.b64encode(data).decode(),'sha256':hashlib.sha256(data).hexdigest()}
root = pathlib.Path(__file__).resolve().parents[1] / 'apps/sites-service/examples/harness-browser'
backend, html = (root/'backend.js').read_bytes(), (root/'index.html').read_bytes()
site = str(uuid.uuid5(a.project, 'harness-browser-demo-v1'))
revision, release = str(uuid.uuid4()), str(uuid.uuid4())
call(a.platform, project_token, f'/api/projects/{a.project}/sites/{site}', 'PUT', {'name':'Harness browser'})
base = f'/internal/sites/{site}'
call(a.sites_service, service_token, base+'/revisions','POST',{'id':revision,'files':[file('backend.ts',backend),file('frontend/index.html',html)]})
call(a.sites_service, service_token, base+'/releases','POST',{'id':release,'manifest':{'source_revision_id':revision,'frontend_entrypoint':'public/index.html','backend_entrypoint':'backend.js','sdk_version':'1'},'files':[file('backend.js',backend),file('public/index.html',html)]})
record = call(a.sites_service, service_token, base)
call(a.sites_service, service_token, base+'/active-release','PUT',{'release_id':release,'expected_generation':record['release_generation']},{'Idempotency-Key':release})
print(f'{a.dashboard}/projects/{a.project}/sites')
