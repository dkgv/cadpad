from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import sys
import traceback

from . import PROTOCOL_VERSION
from .model import DomainError, Model


class BoundedLog(io.TextIOBase):
    def __init__(self):
        self.text = ""
        self.truncated = False

    def write(self, value):
        room = 32768 - len(self.text)
        self.text += value[:max(0, room)]
        self.truncated |= len(value) > room
        return len(value)

    def flush(self):
        pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--generation", required=True)
    options = parser.parse_args()
    # Keep a private protocol descriptor. Native libraries and generated scripts
    # writing fd 1 cannot corrupt newline framing on the private pipe.
    protocol_out = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    model = Model(options.cache)
    for line in sys.stdin:
        request = {}
        log = BoundedLog()
        try:
            if len(line.encode("utf-8")) > 64 * 1024 * 1024:
                raise DomainError("INVALID_REQUEST", "Request exceeds 64 MiB")
            decoded = json.loads(line)
            if not isinstance(decoded, dict):
                raise DomainError("INVALID_REQUEST", "Expected a JSON-RPC request object")
            request = decoded
            if not isinstance(request.get("revision"), str) or not isinstance(request.get("method"), str) or not isinstance(request.get("params", {}), dict):
                raise DomainError("INVALID_REQUEST", "Request requires revision, method and object params")
            if request.get("jsonrpc") != "2.0" or request.get("protocol_version") != PROTOCOL_VERSION:
                raise DomainError("INVALID_REQUEST", "Unsupported private protocol version")
            if request.get("worker_generation") != options.generation:
                raise DomainError("WORKER_UNAVAILABLE", "Worker generation mismatch")
            revision = request["revision"]
            method = request["method"]
            args = request.get("params", {})
            if method not in ("build", "analyze_entry", "index_sources") and revision != model.revision:
                raise DomainError("STALE_ENTITY", "Worker retains a different revision")
            with contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
                if method == "index_sources":
                    from .entrypoint import index_sources
                    result = index_sources(args)
                elif method == "analyze_entry":
                    from .entrypoint import analyze
                    result = analyze(args)
                elif method == "build":
                    result = model.build(args, revision)
                elif method == "inspect":
                    result = model.inspect(args)
                elif method == "remesh":
                    result = model.remesh(args)
                elif method == "export":
                    result = model.export(args)
                else:
                    raise DomainError("UNSUPPORTED_OPERATION", f"Unknown method {method}")
            response = dict(result=result)
        except Exception as error:
            rpc_code = -32700 if isinstance(error, json.JSONDecodeError) else -32600 if getattr(error, "code", None) == "INVALID_REQUEST" else -32000
            data = dict(code=getattr(error, "code", "BUILD_FAILED"), traceback=traceback.format_exc()[-16384:])
            details = getattr(error, "details", None)
            if isinstance(details, dict) and details:
                data.update(details)
            response = dict(error=dict(code=rpc_code, message=str(error), data=data))
        response.update(jsonrpc="2.0", id=request.get("id"), protocol_version=PROTOCOL_VERSION,
                        worker_generation=options.generation, revision=request.get("revision"),
                        logs=log.text, logs_truncated=log.truncated)
        if "error" not in response and method in ("build", "remesh"):
            response["display_cache_policy"] = model._display_policy
        if "error" not in response and method == "build":
            response["geometry_cache"] = model._geometry_cache.manifest() if model._geometry_cache is not None else None
        if "error" in response:
            response["error"]["data"].update(logs=log.text, logs_truncated=log.truncated)
        protocol_out.write(json.dumps(response, allow_nan=False) + "\n")
        protocol_out.flush()


if __name__ == "__main__":
    main()
