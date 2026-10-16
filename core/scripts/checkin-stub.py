#!/usr/bin/env python3
"""Answer every POST /api/checkin with one fixed kind, for live check-in lanes.

    python3 core/scripts/checkin-stub.py --answer enroll --port 8787

Point a waiting host at it with PLOYZ_CLOUD_URL=http://<this host>:8787 in a
ployz-checkin drop-in. It checks nothing, so never expose it beyond a lab.
"""

import argparse
import json
import sys
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWERS = {
    "wait": {"kind": "wait", "retryAfter": 10},
    "enroll": {"kind": "enroll", "instruction": {}},
    "rejected": {
        "kind": "stop",
        "reason": "rejected",
        "message": "This server was rejected in Ployz Cloud.",
    },
    "unknown": {"kind": "reboot"},
}


def handler(answer):
    body = json.dumps(answer).encode()

    class Stub(BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            sent = self.rfile.read(length)
            if self.path != "/api/checkin":
                self.send_error(404)
                return
            try:
                key = json.loads(sent).get("checkinKey", "?")
            except ValueError:
                key = "?"
            stamp = datetime.now(timezone.utc).isoformat(timespec="seconds")
            print(f"{stamp} check-in from {key[:12]} -> {answer['kind']}", flush=True)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format, *args):
            pass

    return Stub


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--answer", choices=sorted(ANSWERS), required=True)
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--bind", default="0.0.0.0")
    args = parser.parse_args()
    server = ThreadingHTTPServer((args.bind, args.port), handler(ANSWERS[args.answer]))
    print(f"answering {args.answer} on {args.bind}:{args.port}", file=sys.stderr, flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
