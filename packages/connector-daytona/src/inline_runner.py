import base64
import difflib
import errno
import fnmatch
import hashlib
import json
import mimetypes
import os
import re
import shutil
import stat
import sys
import tempfile
import time
import traceback
import urllib.parse
import uuid

MAX_INLINE_BYTES = 256 * 1024
PREPARED_LIFETIME_MS = 5 * 60 * 1000
MAX_INLINE_DIFF_BYTES = 64 * 1024


class RunnerError(Exception):
    def __init__(self, code, message, retryable=False, details=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable
        self.details = details or {}


def now_ms():
    return int(time.time() * 1000)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def safe_id(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def cursor_encode(value):
    raw = json.dumps(value, separators=(",", ":"), sort_keys=True).encode("utf-8")
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def cursor_decode(value):
    try:
        padded = value + "=" * (-len(value) % 4)
        return json.loads(base64.urlsafe_b64decode(padded).decode("utf-8"))
    except Exception as exc:
        raise RunnerError("invalid_request", "invalid continuation cursor") from exc


def map_os_error(exc, path=None):
    mapping = {
        errno.ENOENT: "not_found",
        errno.EEXIST: "already_exists",
        errno.EACCES: "permission_denied",
        errno.EPERM: "permission_denied",
        errno.ENOTDIR: "not_directory",
        errno.EISDIR: "is_directory",
        errno.ENOSPC: "resource_exhausted",
        errno.ETIMEDOUT: "deadline_exceeded",
    }
    label = f"{path}: " if path else ""
    return RunnerError(mapping.get(getattr(exc, "errno", None), "internal"), label + str(exc))


class Runtime:
    def __init__(self, envelope):
        self.request = envelope.get("request") or {}
        self.state = envelope["state_directory"]
        self.roots = {root["id"]: root for root in envelope.get("roots", [])}
        self.grants = {grant["id"]: grant for grant in envelope.get("grants", [])}
        os.makedirs(self.state, exist_ok=True)
        self.artifact_dir = os.path.join(self.state, "artifacts")
        self.prepared_dir = os.path.join(self.state, "prepared")
        self.operation_dir = os.path.join(self.state, "operations")
        self.process_dir = os.path.join(self.state, "processes")
        for path in (self.artifact_dir, self.prepared_dir, self.operation_dir, self.process_dir):
            os.makedirs(path, exist_ok=True)

    def _boundary(self, root, candidate):
        try:
            return os.path.commonpath((root, candidate)) == root
        except ValueError:
            return False

    def resolve(self, spec, write=False, follow=True):
        kind = spec.get("type")
        read_only = False
        if kind == "workspace":
            root = self.roots.get(spec.get("root_id"))
            if root is None:
                raise RunnerError("invalid_path", "unknown workspace root")
            root_path = os.path.realpath(root["path"])
            relative = spec.get("path", "")
            if not relative or relative.startswith(("/", "\\")) or "\\" in relative:
                raise RunnerError("invalid_path", "workspace paths must be root-relative POSIX paths")
            parts = relative.split("/")
            if ".." in parts or "\x00" in relative:
                raise RunnerError("invalid_path", "workspace path escapes its root")
            candidate = os.path.normpath(os.path.join(root_path, relative))
            read_only = bool(root.get("read_only"))
        elif kind == "native":
            grant = self.grants.get(spec.get("grant_id"))
            if grant is None:
                raise RunnerError("permission_denied", "native path grant is unknown")
            uri = urllib.parse.urlparse(spec.get("uri", ""))
            if uri.scheme != "file" or uri.netloc not in ("", "localhost"):
                raise RunnerError("unsupported", "Daytona supports only file:// native paths")
            root_path = os.path.realpath(grant["path"])
            candidate = os.path.normpath(urllib.parse.unquote(uri.path))
            read_only = bool(grant.get("read_only"))
        else:
            raise RunnerError("invalid_path", "unknown path specification")
        if write and read_only:
            raise RunnerError("permission_denied", "path belongs to a read-only root")

        if os.path.lexists(candidate):
            checked = os.path.realpath(candidate) if follow else os.path.join(
                os.path.realpath(os.path.dirname(candidate)), os.path.basename(candidate)
            )
        else:
            ancestor = candidate
            suffix = []
            while not os.path.lexists(ancestor):
                parent, name = os.path.split(ancestor)
                if parent == ancestor:
                    break
                suffix.append(name)
                ancestor = parent
            checked = os.path.realpath(ancestor)
            for name in reversed(suffix):
                checked = os.path.join(checked, name)
        if not self._boundary(root_path, checked):
            raise RunnerError("invalid_path", "resolved path escapes its authorized root")
        return checked

    def file_kind(self, mode):
        if stat.S_ISREG(mode):
            return "file"
        if stat.S_ISDIR(mode):
            return "directory"
        if stat.S_ISLNK(mode):
            return "symlink"
        return "other"

    def revision(self, path):
        hasher = hashlib.sha256()
        with open(path, "rb") as handle:
            while True:
                block = handle.read(1024 * 1024)
                if not block:
                    break
                hasher.update(block)
        return hasher.hexdigest()

    def metadata(self, spec, follow=True):
        path = self.resolve(spec, follow=follow)
        try:
            info = os.stat(path, follow_symlinks=follow)
        except OSError as exc:
            raise map_os_error(exc, path)
        kind = self.file_kind(info.st_mode)
        result = {
            "path": spec,
            "kind": kind,
            "size": info.st_size,
            "created_at": int(getattr(info, "st_birthtime", info.st_ctime) * 1000),
            "modified_at": int(info.st_mtime * 1000),
        }
        mime, _ = mimetypes.guess_type(path)
        if mime:
            result["mime_type"] = mime
        if kind == "file":
            result["revision"] = self.revision(path)
        return result

    def directory_entry(self, base_spec, relative, entry):
        spec = append_spec(base_spec, relative)
        try:
            info = entry.stat(follow_symlinks=False)
            kind = self.file_kind(info.st_mode)
            value = {
                "path": spec,
                "name": entry.name,
                "kind": kind,
                "modified_at": int(info.st_mtime * 1000),
            }
            if kind == "file":
                value["size"] = info.st_size
            return value
        except OSError as exc:
            raise map_os_error(exc, entry.path)

    def content(self, source):
        kind = source.get("type")
        if kind == "text":
            return source.get("content", "").encode("utf-8")
        if kind == "base64":
            try:
                return base64.b64decode(source.get("data", ""), validate=True)
            except Exception as exc:
                raise RunnerError("invalid_request", "invalid base64 content") from exc
        if kind == "artifact":
            metadata = self.artifact_metadata(source["artifact_id"])
            with open(metadata["path"], "rb") as handle:
                return handle.read()
        raise RunnerError("invalid_request", "unknown content source")

    def artifact_put(self, data, kind="file", name=None, mime_type=None):
        artifact_id = str(uuid.uuid4())
        path = os.path.join(self.artifact_dir, artifact_id + ".data")
        with open(path, "xb") as handle:
            handle.write(data)
        metadata = {
            "artifact_id": artifact_id,
            "kind": kind,
            "size": len(data),
            "created_at": now_ms(),
            "name": name,
            "mime_type": mime_type,
            "checksum": digest(data),
            "path": path,
        }
        atomic_json(os.path.join(self.artifact_dir, artifact_id + ".json"), metadata)
        return metadata

    def artifact_metadata(self, artifact_id):
        path = os.path.join(self.artifact_dir, safe_id_or_uuid(artifact_id) + ".json")
        # UUID artifact IDs retain their original name; non-UUID IDs are never created here.
        direct = os.path.join(self.artifact_dir, artifact_id + ".json")
        path = direct if os.path.isfile(direct) else path
        try:
            with open(path, "r", encoding="utf-8") as handle:
                return json.load(handle)
        except OSError as exc:
            raise RunnerError("not_found", f"artifact `{artifact_id}` was not found") from exc

    def op_inspect(self):
        return self.metadata(self.request["path"], self.request.get("follow_symlinks", True))

    def op_inspect_many(self):
        items = []
        for spec in self.request.get("paths", []):
            try:
                items.append({"status": "success", "value": self.metadata(spec, self.request.get("follow_symlinks", True))})
            except RunnerError as exc:
                items.append({"status": "error", "error": error_json(exc)})
        return {"items": items}

    def op_read_bytes(self):
        spec = self.request["path"]
        follow = self.request.get("follow_symlinks", True)
        metadata = self.metadata(spec, follow)
        if metadata["kind"] != "file":
            raise RunnerError("is_directory", "byte reads require a regular file")
        span = self.request.get("range")
        offset = span.get("offset", 0) if span else 0
        length = span.get("length", metadata["size"] - offset) if span else metadata["size"] - offset
        if offset > metadata["size"]:
            raise RunnerError("invalid_request", "read offset exceeds file size")
        length = min(length, metadata["size"] - offset)
        path = self.resolve(spec, follow=follow)
        with open(path, "rb") as handle:
            handle.seek(offset)
            data = handle.read(length)
        if len(data) <= MAX_INLINE_BYTES:
            content = {"type": "base64", "data": base64.b64encode(data).decode("ascii")}
            if metadata.get("mime_type"):
                content["mime_type"] = metadata["mime_type"]
        else:
            artifact = self.artifact_put(data, name=os.path.basename(path), mime_type=metadata.get("mime_type"))
            content = {"type": "artifact", "artifact_id": artifact["artifact_id"]}
        return {
            "metadata": metadata,
            "content": content,
            "offset": offset,
            "bytes_returned": len(data),
            "eof": offset + len(data) >= metadata["size"],
        }

    def op_write_bytes(self):
        request = self.request
        spec = request["path"]
        path = self.resolve(spec, write=True, follow=request.get("follow_symlinks", True))
        exists = os.path.lexists(path)
        if exists and os.path.isdir(path):
            raise RunnerError("is_directory", "cannot write bytes to a directory")
        condition = request.get("condition", {"type": "any"})
        if condition["type"] == "must_not_exist" and exists:
            raise RunnerError("already_exists", "target already exists")
        if condition["type"] == "match_revision":
            if not exists or self.revision(path) != condition["revision"]:
                raise RunnerError("stale_revision", "target revision changed")
        data = self.content(request["content"])
        parent = os.path.dirname(path)
        if request.get("create_parents", True):
            os.makedirs(parent, exist_ok=True)
        if not os.path.isdir(parent):
            raise RunnerError("not_directory", "target parent does not exist")
        if request.get("atomic_replace", True):
            atomic_bytes(path, data)
        else:
            with open(path, "wb") as handle:
                handle.write(data)
        return {"path": spec, "existed": exists, "revision": digest(data), "bytes_written": len(data)}

    def op_create_directory(self):
        path = self.resolve(self.request["path"], write=True, follow=False)
        try:
            if self.request.get("recursive", True):
                os.makedirs(path, exist_ok=True)
            else:
                os.mkdir(path)
        except OSError as exc:
            raise map_os_error(exc, path)
        return None

    def op_remove(self):
        request = self.request
        path = self.resolve(request["path"], write=True, follow=False)
        if not os.path.lexists(path):
            if request.get("force", False):
                return None
            raise RunnerError("not_found", "remove target does not exist")
        expected = request.get("expected_revision")
        if expected and (not os.path.isfile(path) or self.revision(path) != expected):
            raise RunnerError("stale_revision", "target revision changed")
        try:
            if os.path.isdir(path) and not os.path.islink(path):
                if request.get("recursive", False):
                    shutil.rmtree(path)
                else:
                    os.rmdir(path)
            else:
                os.unlink(path)
        except OSError as exc:
            raise map_os_error(exc, path)
        return None

    def op_move_path(self):
        request = self.request
        source = self.resolve(request["source"], write=True, follow=False)
        destination = self.resolve(request["destination"], write=True, follow=False)
        expected = request.get("expected_source_revision")
        if expected and (not os.path.isfile(source) or self.revision(source) != expected):
            raise RunnerError("stale_revision", "source revision changed")
        if not request.get("overwrite", False) and os.path.lexists(destination):
            raise RunnerError("already_exists", "destination already exists")
        try:
            (os.replace if request.get("overwrite", False) else os.rename)(source, destination)
        except OSError as exc:
            raise map_os_error(exc, destination)
        return None

    def op_copy_path(self):
        request = self.request
        source = self.resolve(request["source"], follow=False)
        destination = self.resolve(request["destination"], write=True, follow=False)
        if not request.get("overwrite", False) and os.path.lexists(destination):
            raise RunnerError("already_exists", "destination already exists")
        try:
            if os.path.isdir(source) and not os.path.islink(source):
                if not request.get("recursive", False):
                    raise RunnerError("is_directory", "recursive must be enabled to copy a directory")
                shutil.copytree(source, destination, dirs_exist_ok=request.get("overwrite", False))
            else:
                os.makedirs(os.path.dirname(destination), exist_ok=True)
                shutil.copy2(source, destination, follow_symlinks=False)
        except RunnerError:
            raise
        except OSError as exc:
            raise map_os_error(exc, destination)
        return None

    def op_list(self):
        request = self.request
        spec = request["path"]
        metadata = self.metadata(spec, request.get("follow_symlinks", False))
        if metadata["kind"] != "directory":
            raise RunnerError("not_directory", "list requires a directory")
        path = self.resolve(spec, follow=request.get("follow_symlinks", False))
        entries = []
        try:
            with os.scandir(path) as iterator:
                for entry in iterator:
                    if not request.get("include_hidden", True) and entry.name.startswith("."):
                        continue
                    entries.append(self.directory_entry(spec, entry.name, entry))
        except OSError as exc:
            raise map_os_error(exc, path)
        sort = request.get("sort", "name_ascending")
        if sort.startswith("name"):
            entries.sort(key=lambda value: (value["name"].casefold(), value["name"]))
        else:
            entries.sort(key=lambda value: (value.get("modified_at", 0), value["name"]))
        if sort.endswith("descending"):
            entries.reverse()
        offset = cursor_decode(request["cursor"]).get("offset", 0) if request.get("cursor") else 0
        limit = request["limit"]
        page = entries[offset:offset + limit]
        next_offset = offset + len(page)
        truncated = next_offset < len(entries)
        return {
            "directory": metadata,
            "entries": page,
            "truncated": truncated,
            "cursor": cursor_encode({"offset": next_offset}) if truncated else None,
        }

    def op_walk(self):
        request = self.request
        root_spec = request["root"]
        root = self.resolve(root_spec, follow=False)
        if not os.path.isdir(root):
            raise RunnerError("not_directory", "walk requires a directory")
        values = []
        max_depth = request["max_depth"]
        for current, directories, files in os.walk(root, followlinks=request.get("follow_directory_symlinks", False)):
            relative_dir = os.path.relpath(current, root)
            depth = 0 if relative_dir == "." else len(relative_dir.split(os.sep))
            if depth >= max_depth:
                directories[:] = []
            names = list(directories) + list(files)
            names.sort(key=str.casefold)
            for name in names:
                if not request.get("include_hidden", False) and name.startswith("."):
                    continue
                absolute = os.path.join(current, name)
                relative = os.path.relpath(absolute, root).replace(os.sep, "/")
                with os.scandir(current) as iterator:
                    entry = next(item for item in iterator if item.name == name)
                    values.append(self.directory_entry(root_spec, relative, entry))
        offset = cursor_decode(request["cursor"]).get("offset", 0) if request.get("cursor") else 0
        limit = request["max_entries"]
        page = values[offset:offset + limit]
        next_offset = offset + len(page)
        truncated = next_offset < len(values)
        return {"entries": page, "truncated": truncated, "cursor": cursor_encode({"offset": next_offset}) if truncated else None}

    def op_read(self):
        request = self.request
        spec = request["path"]
        metadata = self.metadata(spec, True)
        if metadata["kind"] == "directory":
            raise RunnerError("is_directory", "use list for directory paths")
        if metadata["kind"] != "file":
            raise RunnerError("invalid_request", "read requires a regular file")
        path = self.resolve(spec, follow=True)
        mode = request.get("mode", "auto")
        with open(path, "rb") as handle:
            sample = handle.read(8192)
        media = (metadata.get("mime_type") or "").startswith("image/") or metadata.get("mime_type") == "application/pdf"
        if mode == "media" or (mode == "auto" and media):
            with open(path, "rb") as handle:
                data = handle.read()
            artifact = self.artifact_put(data, "media", os.path.basename(path), metadata.get("mime_type") or "application/octet-stream")
            return {"type": "media", "artifact": {"metadata": metadata, "artifact_id": artifact["artifact_id"], "mime_type": artifact["mime_type"]}}
        if mode == "bytes" or (mode == "auto" and b"\x00" in sample):
            with open(path, "rb") as handle:
                data = handle.read()
            artifact = self.artifact_put(data, "file", os.path.basename(path), metadata.get("mime_type") or "application/octet-stream")
            return {"type": "binary", "artifact": {"metadata": metadata, "artifact_id": artifact["artifact_id"], "mime_type": artifact["mime_type"]}}
        try:
            with open(path, "r", encoding="utf-8", newline="") as handle:
                text = handle.read()
        except UnicodeDecodeError as exc:
            if mode == "auto":
                with open(path, "rb") as handle:
                    data = handle.read()
                artifact = self.artifact_put(data, "file", os.path.basename(path), metadata.get("mime_type") or "application/octet-stream")
                return {"type": "binary", "artifact": {"metadata": metadata, "artifact_id": artifact["artifact_id"], "mime_type": artifact["mime_type"]}}
            raise RunnerError("invalid_request", "file is not valid UTF-8; use bytes mode") from exc
        bounds = request.get("page") or {"max_lines": 2000, "max_bytes": 51200, "max_line_bytes": 8192, "include_total_lines": False}
        if bounds.get("cursor"):
            cursor = cursor_decode(bounds["cursor"])
            if cursor.get("revision") != metadata.get("revision"):
                raise RunnerError("stale_revision", "file changed after the read cursor was created")
            start_line = cursor["line"]
        else:
            start_line = bounds.get("start_line") or 1
        lines = text.splitlines(keepends=True)
        if text and not lines:
            lines = [text]
        index = max(0, start_line - 1)
        returned = []
        used = 0
        lines_truncated = False
        while index < len(lines) and len(returned) < bounds["max_lines"]:
            encoded = lines[index].encode("utf-8")
            if len(encoded) > bounds["max_line_bytes"]:
                encoded = utf8_prefix(encoded, bounds["max_line_bytes"])
                lines_truncated = True
            if used + len(encoded) > bounds["max_bytes"]:
                if not returned:
                    encoded = utf8_prefix(encoded, bounds["max_bytes"])
                    returned.append(encoded.decode("utf-8"))
                    index += 1
                    lines_truncated = True
                break
            returned.append(encoded.decode("utf-8"))
            used += len(encoded)
            index += 1
        has_more = index < len(lines)
        end_line = start_line if not returned else start_line + len(returned) - 1
        page = {
            "metadata": metadata,
            "content": "".join(returned),
            "start_line": start_line,
            "end_line": end_line,
            "has_more": has_more,
            "next_line": end_line + 1 if has_more else None,
            "cursor": cursor_encode({"line": end_line + 1, "revision": metadata.get("revision")}) if has_more else None,
            "lines_truncated": lines_truncated,
        }
        if bounds.get("include_total_lines", False):
            page["total_lines"] = len(lines)
        return {"type": "text", "page": page}

    def op_search(self):
        request = self.request
        root_spec = request["root"]
        root = self.resolve(root_spec, follow=False)
        matcher = compile_matcher(request["pattern"], request["syntax"])
        results = []
        standard_ignored = {".git", "node_modules", "target", ".venv", "__pycache__"}
        for current, directories, files in os.walk(root, followlinks=request.get("follow_symlinks", False)):
            if not request.get("hidden", False):
                directories[:] = [name for name in directories if not name.startswith(".")]
                files = [name for name in files if not name.startswith(".")]
            if request.get("ignore_mode") != "none":
                directories[:] = [name for name in directories if name not in standard_ignored]
            for name in sorted(directories + files, key=str.casefold):
                absolute = os.path.join(current, name)
                relative = os.path.relpath(absolute, root).replace(os.sep, "/")
                if not selected(relative, request.get("include", []), request.get("exclude", [])):
                    continue
                spec = append_spec(root_spec, relative)
                if request["kind"] == "paths":
                    match = matcher(relative)
                    if match:
                        info = os.lstat(absolute)
                        kind = self.file_kind(info.st_mode)
                        entry = {"path": spec, "name": name, "kind": kind, "modified_at": int(info.st_mtime * 1000)}
                        if kind == "file":
                            entry["size"] = info.st_size
                        results.append({"type": "path", "entry": entry})
                    continue
                if not os.path.isfile(absolute):
                    continue
                try:
                    with open(absolute, "r", encoding="utf-8") as handle:
                        lines = handle.read().splitlines()
                except (OSError, UnicodeDecodeError):
                    continue
                byte_offset = 0
                for index, line in enumerate(lines):
                    match = matcher(line)
                    if match:
                        text_bytes = line.encode("utf-8")
                        limited = utf8_prefix(text_bytes, request["max_line_bytes"])
                        start, end = match
                        start_byte = len(line[:start].encode("utf-8"))
                        end_byte = start_byte + len(line[start:end].encode("utf-8"))
                        before_start = max(0, index - request.get("context_before", 0))
                        after_end = min(len(lines), index + 1 + request.get("context_after", 0))
                        results.append({"type": "match", "value": {
                            "path": spec,
                            "line": index + 1,
                            "byte_offset": byte_offset,
                            "text": limited.decode("utf-8"),
                            "before": [utf8_prefix(value.encode("utf-8"), request["max_line_bytes"]).decode("utf-8") for value in lines[before_start:index]],
                            "after": [utf8_prefix(value.encode("utf-8"), request["max_line_bytes"]).decode("utf-8") for value in lines[index + 1:after_end]],
                            "submatches": [{"start": start_byte, "end": end_byte, "text": line[start:end]}],
                            "line_truncated": len(limited) < len(text_bytes),
                        }})
                    byte_offset += len(line.encode("utf-8")) + 1
        offset = cursor_decode(request["cursor"]).get("offset", 0) if request.get("cursor") else 0
        page = []
        used = 0
        index = offset
        while index < len(results) and len(page) < request["max_results"]:
            encoded = len(json.dumps(results[index], separators=(",", ":")).encode("utf-8"))
            if page and used + encoded > request["max_bytes"]:
                break
            page.append(results[index])
            used += encoded
            index += 1
        truncated = index < len(results)
        return {"items": page, "truncated": truncated, "cursor": cursor_encode({"offset": index}) if truncated else None}

    def preview_plan(self, plan):
        if plan.get("post_actions", {}).get("format", "none") != "none" or plan.get("post_actions", {}).get("diagnostics", "none") != "none":
            raise RunnerError("unsupported", "Daytona inline mutations do not yet provide formatter or diagnostics post-actions")
        if not plan.get("operations"):
            raise RunnerError("invalid_request", "mutation plan must not be empty")
        previews = []
        expected = []
        for index, operation in enumerate(plan["operations"]):
            preview, states = self.preview_operation(index, operation)
            previews.append(preview)
            expected.extend(states)
        return previews, expected

    def path_state(self, spec):
        path = self.resolve(spec, follow=False)
        if not os.path.lexists(path):
            return {"path": spec, "exists": False}
        info = os.lstat(path)
        kind = self.file_kind(info.st_mode)
        state = {"path": spec, "exists": True, "kind": kind, "size": info.st_size, "mtime_ns": info.st_mtime_ns}
        if kind == "file":
            state["revision"] = self.revision(path)
        elif kind == "symlink":
            state["target"] = os.readlink(path)
        return state

    def optional_bytes(self, spec):
        path = self.resolve(spec, follow=True)
        if not os.path.exists(path) or not os.path.isfile(path):
            return None
        with open(path, "rb") as handle:
            return handle.read()

    def verify_revision(self, spec, expected):
        if expected is None:
            return
        path = self.resolve(spec, follow=True)
        if not os.path.isfile(path) or self.revision(path) != expected:
            raise RunnerError("stale_revision", "target revision changed")

    def preview_operation(self, index, operation):
        kind = operation["type"]
        if kind in ("put_file", "create_file", "replace_text", "apply_text_patch", "remove"):
            path_spec = operation["path"]
            old = self.optional_bytes(path_spec)
            states = [self.path_state(path_spec)]
            self.verify_revision(path_spec, operation.get("expected_revision"))
            if kind == "put_file":
                new = self.content(operation["content"])
                change = "modified" if old is not None else "created"
            elif kind == "create_file":
                if os.path.lexists(self.resolve(path_spec, follow=False)):
                    raise RunnerError("already_exists", "create target already exists")
                new = self.content(operation["content"])
                change = "created"
            elif kind == "replace_text":
                if old is None:
                    raise RunnerError("not_found", "replace target does not exist")
                new = apply_replacements(old, operation)
                change = "modified"
            elif kind == "apply_text_patch":
                if old is None:
                    raise RunnerError("not_found", "patch target does not exist")
                new = apply_patch(old, operation)
                change = "modified"
            else:
                if not os.path.lexists(self.resolve(path_spec, follow=False)):
                    if operation.get("force", False):
                        old = None
                    else:
                        raise RunnerError("not_found", "remove target does not exist")
                new = None
                change = "removed"
            return mutation_preview(index, path_spec, change, old, new), states
        if kind in ("move", "copy"):
            source = operation["source"]
            destination = operation["destination"]
            if not os.path.lexists(self.resolve(source, follow=False)):
                raise RunnerError("not_found", "mutation source does not exist")
            if kind == "move":
                self.verify_revision(source, operation.get("expected_source_revision"))
            if not operation.get("overwrite", False) and os.path.lexists(self.resolve(destination, follow=False)):
                raise RunnerError("already_exists", "mutation destination already exists")
            states = [self.path_state(source), self.path_state(destination)]
            old = self.optional_bytes(destination)
            new = self.optional_bytes(source)
            return mutation_preview(index, destination, "moved" if kind == "move" else "copied", old, new), states
        raise RunnerError("invalid_request", "unknown mutation operation")

    def verify_expected(self, expected):
        for value in expected:
            current = self.path_state(value["path"])
            if current != value:
                raise RunnerError("stale_revision", "a mutation path changed after preparation")

    def execute_operation(self, operation):
        kind = operation["type"]
        if kind in ("put_file", "create_file"):
            condition = {"type": "must_not_exist"} if kind == "create_file" else (
                {"type": "match_revision", "revision": operation["expected_revision"]}
                if operation.get("expected_revision") else {"type": "any"}
            )
            return self.call_nested("write_bytes", {
                "path": operation["path"], "content": operation["content"], "condition": condition,
                "create_parents": operation.get("create_parents", True), "atomic_replace": True,
                "follow_symlinks": kind != "create_file",
            }), operation["path"], "created" if kind == "create_file" else None
        if kind in ("replace_text", "apply_text_patch"):
            old = self.optional_bytes(operation["path"])
            if old is None:
                raise RunnerError("not_found", "mutation target does not exist")
            new = apply_replacements(old, operation) if kind == "replace_text" else apply_patch(old, operation)
            condition = {"type": "match_revision", "revision": operation["expected_revision"]} if operation.get("expected_revision") else {"type": "any"}
            result = self.call_nested("write_bytes", {
                "path": operation["path"], "content": {"type": "base64", "data": base64.b64encode(new).decode("ascii")},
                "condition": condition, "create_parents": False, "atomic_replace": True, "follow_symlinks": True,
            })
            return result, operation["path"], "modified"
        if kind == "remove":
            self.call_nested("remove", {
                "path": operation["path"], "recursive": operation.get("recursive", False), "force": operation.get("force", False),
                "expected_revision": operation.get("expected_revision"),
            })
            return None, operation["path"], "removed"
        if kind == "move":
            self.call_nested("move_path", {
                "source": operation["source"], "destination": operation["destination"], "overwrite": operation.get("overwrite", False),
                "expected_source_revision": operation.get("expected_source_revision"),
            })
            return None, operation["destination"], "moved"
        if kind == "copy":
            self.call_nested("copy_path", {
                "source": operation["source"], "destination": operation["destination"], "recursive": operation.get("recursive", False),
                "overwrite": operation.get("overwrite", False),
            })
            return None, operation["destination"], "copied"
        raise RunnerError("invalid_request", "unknown mutation operation")

    def call_nested(self, operation, request):
        previous = self.request
        self.request = request
        try:
            return getattr(self, "op_" + operation)()
        finally:
            self.request = previous

    def execute_plan(self, operation_id, plan):
        result_path = os.path.join(self.operation_dir, safe_id(operation_id) + ".json")
        if os.path.isfile(result_path):
            with open(result_path, "r", encoding="utf-8") as handle:
                return json.load(handle)
        self.preview_plan(plan)  # Validate every operation before changing the first path.
        changes = []
        for index, operation in enumerate(plan["operations"]):
            try:
                result, path, forced_change = self.execute_operation(operation)
                revision = result.get("revision") if isinstance(result, dict) else None
                change = forced_change or ("modified" if result and result.get("existed") else "created")
                changes.append({"operation_index": index, "path": path, "change": change, "revision": revision})
            except RunnerError:
                if changes:
                    partial = {"operation_id": operation_id, "status": "partially_committed", "changes": changes, "atomic": False}
                    atomic_json(result_path, partial)
                    return partial
                raise
        result = {"operation_id": operation_id, "status": "committed", "changes": changes, "atomic": False}
        atomic_json(result_path, result)
        return result

    def op_prepare_mutation(self):
        plan = self.request["plan"]
        previews, expected = self.preview_plan(plan)
        prepared_id = str(uuid.uuid4())
        expires_at = now_ms() + PREPARED_LIFETIME_MS
        record = {"prepared_id": prepared_id, "expires_at": expires_at, "plan": plan, "expected": expected, "previews": previews}
        atomic_json(os.path.join(self.prepared_dir, prepared_id + ".json"), record)
        return {"prepared_id": prepared_id, "expires_at": expires_at, "previews": previews, "atomic_commit_supported": False}

    def op_commit_mutation(self):
        prepared_id = self.request["prepared_id"]
        path = os.path.join(self.prepared_dir, safe_id_or_uuid(prepared_id) + ".json")
        direct = os.path.join(self.prepared_dir, prepared_id + ".json")
        path = direct if os.path.isfile(direct) else path
        try:
            with open(path, "r", encoding="utf-8") as handle:
                record = json.load(handle)
        except OSError as exc:
            raise RunnerError("not_found", "prepared mutation was not found") from exc
        if record["expires_at"] < now_ms():
            raise RunnerError("conflict", "prepared mutation expired")
        self.verify_expected(record["expected"])
        result = self.execute_plan(self.request["operation_id"], record["plan"])
        try:
            os.unlink(path)
        except OSError:
            pass
        return result

    def op_abort_mutation(self):
        prepared_id = self.request["prepared_id"]
        path = os.path.join(self.prepared_dir, prepared_id + ".json")
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
        return None

    def op_apply_mutation(self):
        return self.execute_plan(self.request["operation_id"], self.request["plan"])

    def op_artifact_metadata(self):
        metadata = dict(self.artifact_metadata(self.request["artifact_id"]))
        metadata.pop("path", None)
        metadata = {key: value for key, value in metadata.items() if value is not None}
        return metadata

    def op_artifact_open(self):
        metadata = self.artifact_metadata(self.request["artifact_id"])
        offset = self.request.get("offset", 0)
        if offset > metadata["size"]:
            raise RunnerError("invalid_request", "artifact offset exceeds artifact size")
        maximum = min(self.request["max_bytes"], 1024 * 1024)
        with open(metadata["path"], "rb") as handle:
            handle.seek(offset)
            data = handle.read(maximum)
        return {"artifact_id": metadata["artifact_id"], "offset": offset, "data": base64.b64encode(data).decode("ascii"), "eof": offset + len(data) >= metadata["size"]}

    def op_process_replay(self):
        execution_id = self.request["execution_id"]
        after = self.request.get("after_sequence", 0)
        directory = os.path.join(self.process_dir, safe_id(execution_id))
        journal = os.path.join(directory, "events.jsonl")
        events = []
        try:
            with open(journal, "r", encoding="utf-8") as handle:
                for line in handle:
                    value = json.loads(line)
                    if value.get("sequence", 0) > after:
                        events.append(value)
        except FileNotFoundError:
            pass
        status_path = os.path.join(directory, "status.json")
        status = None
        try:
            with open(status_path, "r", encoding="utf-8") as handle:
                status = json.load(handle)
        except FileNotFoundError:
            pass
        return {"events": events, "status": status}

    def op_process_status(self):
        result = self.call_nested("process_replay", {"execution_id": self.request["execution_id"], "after_sequence": 2**63})
        if result["status"] is None:
            raise RunnerError("unknown_execution", "execution was not found")
        return result["status"]

    def dispatch(self, operation):
        method = getattr(self, "op_" + operation, None)
        if method is None:
            raise RunnerError("unsupported", f"inline operation `{operation}` is unsupported")
        return method()


def append_spec(spec, relative):
    result = dict(spec)
    if spec["type"] == "workspace":
        base = spec.get("path", ".")
        result["path"] = relative if base == "." else base.rstrip("/") + "/" + relative
    else:
        result["uri"] = spec["uri"].rstrip("/") + "/" + urllib.parse.quote(relative, safe="/")
    return result


def selected(path, includes, excludes):
    if includes and not any(fnmatch.fnmatch(path, value) for value in includes):
        return False
    return not any(fnmatch.fnmatch(path, value) for value in excludes)


def compile_matcher(pattern, syntax):
    if syntax == "regex":
        try:
            expression = re.compile(pattern)
        except re.error as exc:
            raise RunnerError("invalid_request", f"invalid regex: {exc}") from exc
        return lambda value: ((match.start(), match.end()) if (match := expression.search(value)) else None)
    if syntax == "glob":
        expression = re.compile(fnmatch.translate(pattern))
        return lambda value: ((match.start(), match.end()) if (match := expression.search(value)) else None)
    return lambda value: ((index, index + len(pattern)) if (index := value.find(pattern)) >= 0 else None)


def utf8_prefix(data, limit):
    value = data[:limit]
    while value:
        try:
            value.decode("utf-8")
            return value
        except UnicodeDecodeError:
            value = value[:-1]
    return b""


def atomic_bytes(path, data):
    parent = os.path.dirname(path)
    descriptor, temporary = tempfile.mkstemp(prefix=".agent-pane-", dir=parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def atomic_json(path, value):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    atomic_bytes(path, json.dumps(value, separators=(",", ":"), sort_keys=True).encode("utf-8"))


def error_json(exc):
    return {"code": exc.code, "message": exc.message, "retryable": exc.retryable, "details": exc.details}


def safe_id_or_uuid(value):
    return value if re.fullmatch(r"[0-9a-fA-F-]{32,36}", value or "") else safe_id(value or "")


def apply_replacements(old, operation):
    bom = old.startswith(b"\xef\xbb\xbf")
    body = old[3:] if bom else old
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise RunnerError("invalid_request", "replace_text requires UTF-8") from exc
    for replacement in operation.get("replacements", []):
        source = replacement["old_text"]
        count = text.count(source)
        policy = replacement["occurrence"]
        if count == 0:
            raise RunnerError("conflict", "replacement text was not found")
        if policy == "unique" and count != 1:
            raise RunnerError("conflict", "replacement text is not unique")
        text = text.replace(source, replacement["new_text"], 1 if policy in ("unique", "first") else -1)
    result = text.encode("utf-8")
    return (b"\xef\xbb\xbf" + result) if bom and operation.get("preserve_utf8_bom", True) else result


def apply_patch(old, operation):
    bom = old.startswith(b"\xef\xbb\xbf")
    body = old[3:] if bom else old
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise RunnerError("invalid_request", "text patch requires UTF-8") from exc
    newline = "\r\n" if operation.get("preserve_line_endings", True) and "\r\n" in text else "\n"
    lines = text.replace("\r\n", "\n").split("\n")
    if lines and lines[-1] == "":
        lines.pop()
        trailing = True
    else:
        trailing = False
    position = 0
    for hunk in operation.get("hunks", []):
        old_lines = hunk.get("old_lines", [])
        candidates = []
        for index in range(position, len(lines) - len(old_lines) + 1):
            existing = lines[index:index + len(old_lines)]
            if operation.get("match_policy") == "whitespace_tolerant":
                matches = [value.strip() for value in existing] == [value.strip() for value in old_lines]
            else:
                matches = existing == old_lines
            if matches:
                candidates.append(index)
        if not candidates:
            raise RunnerError("conflict", "patch hunk did not match")
        index = candidates[0]
        lines[index:index + len(old_lines)] = hunk.get("new_lines", [])
        position = index + len(hunk.get("new_lines", []))
    result_text = newline.join(lines) + (newline if trailing else "")
    result = result_text.encode("utf-8")
    return (b"\xef\xbb\xbf" + result) if bom and operation.get("preserve_utf8_bom", True) else result


def mutation_preview(index, path, change, old, new):
    old_text = (old or b"").decode("utf-8", errors="replace").splitlines(keepends=True)
    new_text = (new or b"").decode("utf-8", errors="replace").splitlines(keepends=True)
    diff = "".join(difflib.unified_diff(old_text, new_text, fromfile="before", tofile="after"))
    encoded = diff.encode("utf-8")
    if len(encoded) > MAX_INLINE_DIFF_BYTES:
        diff = utf8_prefix(encoded, MAX_INLINE_DIFF_BYTES).decode("utf-8") + "\n... diff truncated ...\n"
    value = {"operation_index": index, "path": path, "change": change, "diff": diff}
    if old is not None:
        value["base_revision"] = digest(old)
    return value


def main():
    try:
        packed = base64.b64decode(sys.argv[1])
        envelope = json.loads(__import__("zlib").decompress(packed).decode("utf-8"))
        runtime = Runtime(envelope)
        result = runtime.dispatch(envelope["operation"])
        output = {"ok": True, "value": result}
    except RunnerError as exc:
        output = {"ok": False, "error": error_json(exc)}
    except Exception as exc:
        output = {"ok": False, "error": {
            "code": "internal", "message": str(exc), "retryable": False,
            "details": {"traceback": traceback.format_exc(limit=8)},
        }}
    sys.stdout.write(json.dumps(output, separators=(",", ":")))
    sys.stdout.flush()


if __name__ == "__main__":
    main()
