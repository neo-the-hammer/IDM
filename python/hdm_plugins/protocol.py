"""The daemon speaks to this package in JSON lines over stdio.

One request per line in, one response per line out. Line-delimited rather than
length-prefixed because it is trivial to drive by hand when something goes
wrong -- `echo '{"action":"ping"}' | python3 -m hdm_plugins` is a complete
debugging session.

Every response carries `ok`. A failure is a reply, never a traceback on stdout
and never a non-zero exit: the daemon has to be able to tell "this page had no
links" from "the plugin host is broken", and a crash makes them look alike.
"""

from __future__ import annotations

import json
import sys
import traceback
from typing import Any, Callable

Handler = Callable[[dict], dict]

#: Refuse inputs larger than this. A hostile or broken page should not be able
#: to make the plugin host allocate without bound.
MAX_REQUEST_BYTES = 32 * 1024 * 1024


def ok(**fields: Any) -> dict:
    return {"ok": True, **fields}


def error(message: str, **fields: Any) -> dict:
    return {"ok": False, "error": message, **fields}


def serve(handlers: dict[str, Handler], stdin=None, stdout=None) -> int:
    """Reads requests until the stream ends, writing one response each."""
    stdin = stdin or sys.stdin
    stdout = stdout or sys.stdout

    for line in stdin:
        line = line.strip()
        if not line:
            continue
        if len(line) > MAX_REQUEST_BYTES:
            respond(stdout, error("request too large"))
            continue

        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            respond(stdout, error(f"malformed request: {exc}"))
            continue

        if not isinstance(request, dict):
            respond(stdout, error("request must be a JSON object"))
            continue

        action = request.get("action", "")
        handler = handlers.get(action)
        if handler is None:
            respond(stdout, error(f"unknown action `{action}`", action=action))
            continue

        try:
            response = handler(request)
        except Exception as exc:  # noqa: BLE001 - a plugin fault must not kill the host
            # The traceback goes to stderr, where the daemon logs it, while the
            # caller gets a reply it can act on.
            traceback.print_exc(file=sys.stderr)
            response = error(f"{type(exc).__name__}: {exc}")

        # Echo the request id so the daemon can match replies even if it ever
        # pipelines requests.
        if "id" in request:
            response.setdefault("id", request["id"])
        respond(stdout, response)
    return 0


def respond(stdout, payload: dict) -> None:
    # No embedded newlines, or the framing breaks.
    json.dump(payload, stdout, ensure_ascii=False, separators=(",", ":"))
    stdout.write("\n")
    stdout.flush()
