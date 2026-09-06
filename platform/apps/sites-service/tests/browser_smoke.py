"""Manual browser check: cargo build first, run this file, open its printed URL.

Uses only temporary storage and the Python standard library. Exits successfully
when a real browser confirms module/CSS loading and iframe restrictions.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen
import uuid


def main():
    completed = threading.Event()
    result = {}
    page = {"html": "Preparing fixture"}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(page["html"].encode())

        def do_POST(self):
            result.update(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            self.send_response(204)
            self.end_headers()
            completed.set()

    dashboard = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=dashboard.serve_forever, daemon=True).start()
    dashboard_origin = f"http://127.0.0.1:{dashboard.server_port}"
    reservations = [socket.socket(), socket.socket()]
    for sock in reservations:
        sock.bind(("127.0.0.1", 0))
    api_port, content_port = [sock.getsockname()[1] for sock in reservations]
    for sock in reservations:
        sock.close()
    token = "browser-smoke-only-token-01234567890123456789"
    api = f"http://127.0.0.1:{api_port}"

    def call(method, path, body=None):
        encoded = None if body is None else json.dumps(body).encode()
        req = Request(api + path, data=encoded, method=method,
                      headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        with urlopen(req, timeout=10) as response:
            return json.load(response)

    def file(path, data):
        data = data.encode()
        return {"path": path, "content_base64": base64.b64encode(data).decode(),
                "sha256": hashlib.sha256(data).hexdigest()}

    try:
        with tempfile.TemporaryDirectory(prefix="sites-browser-") as directory:
            env = dict(os.environ, SITES_DATA_DIR=directory, SITES_API_TOKEN=token,
                       SITES_BIND_ADDRESS=f"127.0.0.1:{api_port}",
                       SITES_CONTENT_BIND_ADDRESS=f"127.0.0.1:{content_port}",
                       SITES_CONTENT_ORIGIN=f"http://127.0.0.1:{content_port}",
                       SITES_DASHBOARD_ORIGIN=dashboard_origin, RUST_LOG="error")
            executable = Path(__file__).resolve().parents[3] / "target/debug/platform-sites-service"
            process = subprocess.Popen([str(executable)], env=env, stdout=subprocess.DEVNULL)
            try:
                for _ in range(100):
                    try:
                        call("GET", "/readyz")
                        break
                    except OSError:
                        if process.poll() is not None:
                            raise RuntimeError("Sites service exited")
                        time.sleep(0.1)
                else:
                    raise RuntimeError("Sites service never became ready")
                site, source, release = [str(uuid.uuid4()) for _ in range(3)]
                root = f"/internal/sites/{site}"
                call("PUT", root, {"project_id": str(uuid.uuid4())})
                for _ in range(100):
                    if call("GET", root)["status"] == "ready":
                        break
                    time.sleep(0.1)
                call("POST", root + "/revisions", {"id": source, "files": [
                    file("backend.ts", "export default () => ({ok: true});"),
                    file("frontend/index.html", "<html>source</html>")]})
                script = """
import {value} from './dep.js';
let parentBlocked = false, storageBlocked = false, fetchBlocked = false;
try { parent.document.body; } catch { parentBlocked = true; }
try { localStorage.getItem('test'); } catch { storageBlocked = true; }
try { await fetch('https://example.com/'); } catch { fetchBlocked = true; }
parent.postMessage({kind: 'site-smoke', module: value,
  color: getComputedStyle(document.body).color,
  parentBlocked, storageBlocked, fetchBlocked}, DASHBOARD);
""".replace("DASHBOARD", json.dumps(dashboard_origin))
                call("POST", root + "/releases", {"id": release, "manifest": {
                    "source_revision_id": source, "frontend_entrypoint": "public/index.html",
                    "backend_entrypoint": "backend.js", "sdk_version": "1"}, "files": [
                        file("backend.js", "export default () => ({ok:true});"),
                        file("public/index.html", '<!doctype html><title>Site iframe fixture</title><link rel="stylesheet" href="styles.css"><script type="module" src="app.js"></script><body>Site iframe fixture</body>'),
                        file("public/styles.css", "body { color: rgb(0, 51, 102); }"),
                        file("public/app.js", script), file("public/dep.js", "export const value = 'loaded';")
                    ]})
                access = call("POST", root + f"/releases/{release}/content-access", {})
                page["html"] = """<!doctype html><title>Sites service browser verification</title>
<h1>Sites service browser verification</h1><pre id="result">Waiting for iframe…</pre>
<iframe id="site" sandbox="allow-scripts"></iframe>
<script>
const frame = document.getElementById('site');
addEventListener('message', event => {
  if (event.source !== frame.contentWindow || event.data?.kind !== 'site-smoke') return;
  const result = {...event.data, opaqueOrigin: event.origin === 'null'};
  document.getElementById('result').textContent = JSON.stringify(result, null, 2);
  fetch('/result', {method:'POST', body:JSON.stringify(result)});
});
frame.src = CONTENT_URL;
</script>""".replace("CONTENT_URL", json.dumps(access["url"]))
                print("Open in Chrome:", dashboard_origin, flush=True)
                if not completed.wait(120):
                    raise RuntimeError("Browser did not report its result within 120 seconds")
                expected = {"kind": "site-smoke", "module": "loaded", "color": "rgb(0, 51, 102)",
                            "parentBlocked": True, "storageBlocked": True, "fetchBlocked": True,
                            "opaqueOrigin": True}
                if result != expected:
                    raise AssertionError(result)
                print("PASS:", json.dumps(result), flush=True)
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    finally:
        dashboard.shutdown()
        dashboard.server_close()


if __name__ == "__main__":
    main()
