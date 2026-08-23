import base64
import fcntl
import hashlib
import json
import math
import os
import pty
import resource
import selectors
import signal
import struct
import subprocess
import sys
import termios
import time
import traceback
import uuid
import zlib

ACTIVE_JOURNAL = None
ACTIVE_CHILD = None


def now_ms():
    return int(time.time() * 1000)


def safe_id(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def atomic_json(path, value):
    temporary = path + ".tmp-" + str(os.getpid())
    with open(temporary, "w", encoding="utf-8") as handle:
        json.dump(value, handle, separators=(",", ":"), sort_keys=True)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def resolve_path(spec, roots, grants):
    if spec["type"] == "workspace":
        root = roots[spec["root_id"]]
        relative = spec["path"]
        if relative.startswith(("/", "\\")) or ".." in relative.split("/") or "\\" in relative:
            raise ValueError("cwd escapes its workspace root")
        root_path = os.path.realpath(root["path"])
        path = os.path.realpath(os.path.join(root_path, relative))
    else:
        import urllib.parse
        grant = grants[spec["grant_id"]]
        root_path = os.path.realpath(grant["path"])
        uri = urllib.parse.urlparse(spec["uri"])
        if uri.scheme != "file":
            raise ValueError("only file:// native cwd paths are supported")
        path = os.path.realpath(urllib.parse.unquote(uri.path))
    if os.path.commonpath((root_path, path)) != root_path:
        raise ValueError("cwd escapes its authorized root")
    return path


def environment_for(request):
    config = request.get("environment", {})
    inherit = config.get("inherit", "all")
    if inherit == "none":
        env = {}
    elif inherit == "allow_list":
        env = {key: value for key, value in os.environ.items() if key in config.get("allow", [])}
    else:
        env = dict(os.environ)
    for key in config.get("remove", []):
        env.pop(key, None)
    env.update(config.get("set", {}))
    return env


def command_for(request):
    command = request["command"]
    if command["type"] == "argv":
        return [command["program"]] + command.get("arguments", [])
    shell = command.get("shell") or "/bin/bash"
    arguments = ["-c", command["command"]]
    if command.get("login", False):
        arguments.insert(0, "-l")
    return [shell] + arguments


def limit_preexec(limits, pty_slave=None):
    def apply():
        os.setsid()
        if pty_slave is not None:
            fcntl.ioctl(pty_slave, termios.TIOCSCTTY, 0)
        if not limits:
            return
        memory = limits.get("memory_bytes")
        if memory:
            resource.setrlimit(resource.RLIMIT_AS, (memory, memory))
        cpu_ms = limits.get("cpu_time_ms")
        if cpu_ms:
            seconds = max(1, math.ceil(cpu_ms / 1000))
            resource.setrlimit(resource.RLIMIT_CPU, (seconds, seconds))
        count = limits.get("process_count")
        if count:
            resource.setrlimit(resource.RLIMIT_NPROC, (count, count))
    return apply


class Journal:
    def __init__(self, request, directory, state_directory):
        self.execution_id = request["execution_id"]
        self.sequence = 0
        self.directory = directory
        self.path = os.path.join(directory, "events.jsonl")
        self.status_path = os.path.join(directory, "status.json")
        self.handle = open(self.path, "a", encoding="utf-8", buffering=1)
        self.started_at = now_ms()
        self.output_artifact_id = None
        self.output_path = None
        self.output_handle = None
        self.closed = False
        if request.get("output", {}).get("persist_full_output", True):
            self.output_artifact_id = str(uuid.uuid4())
            artifact_directory = os.path.join(state_directory, "artifacts")
            os.makedirs(artifact_directory, exist_ok=True)
            self.output_path = os.path.join(artifact_directory, self.output_artifact_id + ".data")
            self.output_handle = open(self.output_path, "xb")

    def emit(self, event):
        if event.get("type") == "output" and self.output_handle is not None:
            self.output_handle.write(base64.b64decode(event["data"]))
            self.output_handle.flush()
        self.sequence += 1
        value = {
            "execution_id": self.execution_id,
            "sequence": self.sequence,
            "timestamp": now_ms(),
            "event": event,
        }
        line = json.dumps(value, separators=(",", ":")) + "\n"
        self.handle.write(line)
        self.handle.flush()
        sys.stdout.write(line)
        sys.stdout.flush()
        if event.get("type") == "closed":
            self.closed = True
        return value

    def status(self, state, exit_code=None):
        value = {
            "execution_id": self.execution_id,
            "state": state,
            "started_at": self.started_at,
            "last_sequence": self.sequence,
        }
        if state in ("exited", "failed", "cancelled", "lost"):
            value["finished_at"] = now_ms()
        if exit_code is not None:
            value["exit_code"] = exit_code
        if self.output_artifact_id is not None:
            value["full_output_artifact"] = self.output_artifact_id
        atomic_json(self.status_path, value)
        return value

    def finalize_output(self):
        if self.output_handle is None:
            return
        self.output_handle.flush()
        os.fsync(self.output_handle.fileno())
        self.output_handle.close()
        self.output_handle = None
        with open(self.output_path, "rb") as handle:
            data = handle.read()
        metadata = {
            "artifact_id": self.output_artifact_id,
            "kind": "process_output",
            "size": len(data),
            "created_at": self.started_at,
            "name": self.execution_id + ".output",
            "mime_type": "application/octet-stream",
            "checksum": hashlib.sha256(data).hexdigest(),
            "path": self.output_path,
        }
        atomic_json(os.path.join(os.path.dirname(self.output_path), self.output_artifact_id + ".json"), metadata)


def set_pty_size(fd, columns, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def signal_group(child, name):
    values = {
        "interrupt": signal.SIGINT,
        "terminate": signal.SIGTERM,
        "kill": signal.SIGKILL,
        "hangup": signal.SIGHUP,
        "user1": signal.SIGUSR1,
        "user2": signal.SIGUSR2,
    }
    os.killpg(child.pid, values[name])


def main():
    global ACTIVE_JOURNAL, ACTIVE_CHILD
    envelope = json.loads(zlib.decompress(base64.b64decode(sys.argv[1])).decode("utf-8"))
    request = envelope["request"]
    roots = {root["id"]: root for root in envelope.get("roots", [])}
    grants = {grant["id"]: grant for grant in envelope.get("grants", [])}
    directory = os.path.join(envelope["state_directory"], "processes", safe_id(request["execution_id"]))
    os.makedirs(directory, exist_ok=True)
    # Tensorlake's v1 REST process records have no user tag/name. Persist the
    # outer wrapper PID beside the journal so a new connector instance can
    # reattach to its captured-and-followed output stream.
    atomic_json(os.path.join(directory, "runner.json"), {
        "pid": os.getpid(),
        "execution_id": request["execution_id"],
    })
    journal = Journal(request, directory, envelope["state_directory"])
    ACTIVE_JOURNAL = journal
    write_ids_path = os.path.join(directory, "write_ids")
    try:
        with open(write_ids_path, "r", encoding="utf-8") as handle:
            write_ids = set(handle.read().splitlines())
    except FileNotFoundError:
        write_ids = set()

    cwd = resolve_path(request["cwd"], roots, grants)
    env = environment_for(request)
    argv = command_for(request)
    stdin_mode = request["stdin"]["type"]
    limits = (request.get("policy") or {}).get("resource_limits")
    maximum = max(1, min(request.get("output", {}).get("max_chunk_bytes", 65536), 1024 * 1024))
    pty_master = None
    child_stdin = None
    if stdin_mode == "pty":
        pty_master, slave = pty.openpty()
        set_pty_size(pty_master, request["stdin"]["columns"], request["stdin"]["rows"])
        child = subprocess.Popen(
            argv, cwd=cwd, env=env, stdin=slave, stdout=slave, stderr=slave,
            close_fds=True, preexec_fn=limit_preexec(limits, slave),
        )
        os.close(slave)
        os.set_blocking(pty_master, False)
    else:
        child = subprocess.Popen(
            argv, cwd=cwd, env=env,
            stdin=subprocess.PIPE if stdin_mode == "pipe" else subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            bufsize=0, close_fds=True, preexec_fn=limit_preexec(limits),
        )
        child_stdin = child.stdin
        os.set_blocking(child.stdout.fileno(), False)
        os.set_blocking(child.stderr.fileno(), False)
    ACTIVE_CHILD = child

    journal.emit({"type": "started"})
    journal.status("running")
    selector = selectors.DefaultSelector()
    os.set_blocking(sys.stdin.fileno(), False)
    selector.register(sys.stdin.fileno(), selectors.EVENT_READ, "control")
    if pty_master is not None:
        selector.register(pty_master, selectors.EVENT_READ, "pty")
    else:
        selector.register(child.stdout.fileno(), selectors.EVENT_READ, "stdout")
        selector.register(child.stderr.fileno(), selectors.EVENT_READ, "stderr")

    control_buffer = bytearray()
    deadline = None
    if request.get("timeout_ms"):
        deadline = time.monotonic() + request["timeout_ms"] / 1000
    cancelled = False
    open_outputs = 1 if pty_master is not None else 2

    while child.poll() is None or open_outputs:
        if deadline is not None and time.monotonic() >= deadline and child.poll() is None:
            signal_group(child, "kill")
            cancelled = True
        for key, _ in selector.select(0.1):
            if key.data == "control":
                try:
                    chunk = os.read(sys.stdin.fileno(), 65536)
                except BlockingIOError:
                    continue
                if not chunk:
                    try:
                        selector.unregister(sys.stdin.fileno())
                    except Exception:
                        pass
                    continue
                control_buffer.extend(chunk)
                while b"\n" in control_buffer:
                    line, _, rest = control_buffer.partition(b"\n")
                    control_buffer = bytearray(rest)
                    if not line:
                        continue
                    command = json.loads(line)
                    kind = command["type"]
                    if kind == "input":
                        write_id = command["write_id"]
                        if write_id in write_ids:
                            continue
                        data = base64.b64decode(command["data"])
                        if pty_master is not None:
                            os.write(pty_master, data)
                        elif child_stdin is not None and not child_stdin.closed:
                            child_stdin.write(data)
                            child_stdin.flush()
                        else:
                            continue
                        write_ids.add(write_id)
                        with open(write_ids_path, "a", encoding="utf-8") as handle:
                            handle.write(write_id + "\n")
                            handle.flush()
                    elif kind == "resize" and pty_master is not None:
                        set_pty_size(pty_master, command["columns"], command["rows"])
                        os.killpg(child.pid, signal.SIGWINCH)
                    elif kind == "signal" and child.poll() is None:
                        signal_group(child, command["signal"])
                        cancelled = command["signal"] in ("terminate", "kill")
                    elif kind == "close_stdin":
                        if child_stdin is not None and not child_stdin.closed:
                            child_stdin.close()
                    elif kind == "terminate" and child.poll() is None:
                        signal_group(child, "terminate")
                        cancelled = True
            else:
                try:
                    data = os.read(key.fd, maximum)
                except (BlockingIOError, OSError):
                    data = b""
                if data:
                    journal.emit({
                        "type": "output", "stream": key.data,
                        "data": base64.b64encode(data).decode("ascii"),
                    })
                else:
                    try:
                        selector.unregister(key.fd)
                    except Exception:
                        pass
                    open_outputs -= 1

    return_code = child.wait()
    exit_code = return_code if return_code >= 0 else 128 + abs(return_code)
    if cancelled:
        journal.emit({"type": "exited", "exit_code": exit_code, "sandbox_denied": False})
        journal.status("cancelled", exit_code)
    else:
        journal.emit({"type": "exited", "exit_code": exit_code, "sandbox_denied": False})
        journal.status("exited", exit_code)
    journal.emit({"type": "closed"})
    journal.finalize_output()
    # Preserve the terminal status while including the final Closed sequence.
    journal.status("cancelled" if cancelled else "exited", exit_code)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        try:
            if ACTIVE_CHILD is not None and ACTIVE_CHILD.poll() is None:
                os.killpg(ACTIVE_CHILD.pid, signal.SIGKILL)
        except Exception:
            pass
        try:
            if ACTIVE_JOURNAL is not None and not ACTIVE_JOURNAL.closed:
                ACTIVE_JOURNAL.emit({"type": "failed", "message": str(exc)})
                ACTIVE_JOURNAL.emit({"type": "closed"})
                ACTIVE_JOURNAL.finalize_output()
                ACTIVE_JOURNAL.status("failed")
        except Exception:
            pass
        sys.stderr.write(str(exc) + "\n" + traceback.format_exc(limit=12))
        sys.stderr.flush()
        raise
