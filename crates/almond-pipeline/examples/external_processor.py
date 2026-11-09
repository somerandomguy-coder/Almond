#!/usr/bin/env python3
"""Example Almond external processor (JSON-RPC 2.0). Standard library only.

Implements one processor, ``stats.talk_time``: consumes a ``transcript`` artifact and produces a
``talk_time`` artifact with speaking time per speaker, each row citing the segments it counts.

Transports
----------
stdio (default):  one JSON message per line on stdin/stdout; logs go to stderr.
    The host spawns:  python external_processor.py
HTTP:             python external_processor.py --http 8765
    The host POSTs JSON-RPC requests to http://127.0.0.1:8765/ (no progress notifications).

Methods (see docs/API.md "External processors"):
    processor.describe -> ProcessorDescriptor[]
    processor.run {processor, job_id, meeting, params, inputs} -> ArtifactDraft[]
Notifications:
    processor.progress {job_id, progress, message}   (processor -> host, stdio only)
    processor.cancel   {job_id}                      (host -> processor)

Use this as a template for research processors (Z3 / Prolog / NLI verifiers, ...).
"""

import json
import sys
import threading

VERSION = "0.1.0"

DESCRIPTORS = [
    {
        "name": "stats.talk_time",
        "version": VERSION,
        "consumes": ["transcript"],
        "produces": ["talk_time"],
        "needs_audio": False,
        "requires": [],
        "description": "Speaking time per speaker (example external processor)",
    }
]

CANCELLED = set()
_out_lock = threading.Lock()


def log(*a):
    print(*a, file=sys.stderr, flush=True)


class RpcError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


def send(msg):
    """Write one JSON-RPC message to stdout (stdio transport)."""
    with _out_lock:
        sys.stdout.write(json.dumps(msg, ensure_ascii=False) + "\n")
        sys.stdout.flush()


def talk_time(params, notify):
    job_id = params["job_id"]
    meeting = params["meeting"]
    transcripts = [a for a in params.get("inputs", []) if a.get("kind") == "transcript"]
    if not transcripts:
        raise RpcError(-32602, "no transcript among inputs")
    t = transcripts[0]
    segments = t["body"]["segments"]
    rows = {}
    for i, s in enumerate(segments):
        if job_id in CANCELLED:
            raise RpcError(-32000, "cancelled")
        who = s.get("speaker") or s.get("channel") or "unknown"
        r = rows.setdefault(who, {"speaker": who, "ms": 0, "segments": 0, "words": 0, "cites": []})
        r["ms"] += max(0, s["t1_ms"] - s["t0_ms"])
        r["segments"] += 1
        r["words"] += len(s["text"].split())
        r["cites"].append(
            {"artifact": t["id"], "segment": s["idx"], "chars": None, "time_ms": [s["t0_ms"], s["t1_ms"]]}
        )
        notify({"job_id": job_id, "progress": (i + 1) / len(segments), "message": f"segment {i + 1}/{len(segments)}"})
    total = sum(r["ms"] for r in rows.values()) or 1
    speakers = sorted(rows.values(), key=lambda r: -r["ms"])
    for r in speakers:
        r["share"] = round(r["ms"] / total, 4)
    return [
        {
            "meeting_id": meeting["id"],
            "kind": "talk_time",
            "schema": 1,
            "body": {"total_ms": total, "speakers": speakers},
            "provenance": {
                "processor": params["processor"],
                "processor_version": VERSION,
                "model": None,
                "params_hash": None,
                "inputs": [t["id"]],
            },
            "supersedes": None,
        }
    ]


def dispatch(method, params, notify):
    if method == "processor.describe":
        return DESCRIPTORS
    if method == "processor.run":
        name = params.get("processor")
        if name == "stats.talk_time":
            return talk_time(params, notify)
        raise RpcError(-32004, f"unknown processor {name!r}")
    raise RpcError(-32601, method)


def handle(msg, notify):
    """Handle one decoded message; returns a response dict or None for notifications."""
    method = msg.get("method")
    if "id" not in msg:  # notification
        if method == "processor.cancel":
            CANCELLED.add(msg.get("params", {}).get("job_id"))
        return None
    try:
        result = dispatch(method, msg.get("params") or {}, notify)
        return {"jsonrpc": "2.0", "id": msg["id"], "result": result}
    except RpcError as e:
        return {"jsonrpc": "2.0", "id": msg["id"], "error": {"code": e.code, "message": e.message}}
    except Exception as e:  # noqa: BLE001 - report everything to the host
        return {"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32000, "message": repr(e)}}


def serve_stdio():
    log("external_processor: stdio ready")

    def notify(p):
        send({"jsonrpc": "2.0", "method": "processor.progress", "params": p})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            send({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}})
            continue
        # Run each request on a thread so `processor.cancel` can arrive while a job runs.
        def work(m=msg):
            resp = handle(m, notify)
            if resp is not None:
                send(resp)

        if msg.get("method") == "processor.run":
            threading.Thread(target=work, daemon=True).start()
        else:
            work()


def serve_http(port):
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    class H(BaseHTTPRequestHandler):
        def do_POST(self):  # noqa: N802
            n = int(self.headers.get("Content-Length", "0"))
            try:
                msg = json.loads(self.rfile.read(n) or b"{}")
            except json.JSONDecodeError:
                msg = None
            if msg is None:
                resp = {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}}
            else:
                resp = handle(msg, lambda p: None) or {"jsonrpc": "2.0", "id": None, "result": None}
            body = json.dumps(resp, ensure_ascii=False).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    srv = ThreadingHTTPServer(("127.0.0.1", port), H)
    # The host waits for this line before connecting.
    print(f"listening on http://127.0.0.1:{srv.server_address[1]}/", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "--http":
        serve_http(int(sys.argv[2]))
    else:
        serve_stdio()
