#!/usr/bin/env python3
"""Bounded loopback Cloud enrollment fixture for release qualification."""

import http.server
import json
import re
import sys

port_file, evidence_file = sys.argv[1:]
token = "pmet_qualification"
secret = "qualification-synthetic-pairing-secret-v1"
expected_paths = [f"/api/enroll/{token}", f"/api/enroll/{token}/callback"]


class Handler(http.server.BaseHTTPRequestHandler):
    requests = 0
    failure = None

    def do_POST(self):
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > 1024 * 1024:
                raise AssertionError(f"invalid request size {length}")
            body = json.loads(self.rfile.read(length))
            expected = expected_paths[Handler.requests]
            if self.path != expected:
                raise AssertionError(
                    f"request {Handler.requests + 1} path {self.path!r}, expected {expected!r}"
                )
            if Handler.requests == 0:
                if body.get("protocolVersion") != 2:
                    raise AssertionError("enroll protocolVersion was not 2")
                if body.get("name") != "qualify-1":
                    raise AssertionError("enroll Machine Name was not qualify-1")
                if body.get("requestedStorage") != "none":
                    raise AssertionError("enroll requestedStorage was not none")
                if not isinstance(body.get("publicKey"), str) or not body["publicKey"]:
                    raise AssertionError("enroll publicKey was empty")
                response = {
                    "kind": "initialize",
                    "resumed": True,
                    "storage": "none",
                    "pairing": {
                        "secret": secret,
                    },
                }
                evidence = {"request": "enroll", "identity": "accepted"}
            else:
                machine_id = body.get("machineId")
                valid_machine_id = isinstance(
                    machine_id, str
                ) and re.fullmatch(r"[0-9a-f]{32}", machine_id)
                if not valid_machine_id:
                    raise AssertionError(
                        "callback Machine ID was not 32 lowercase hex characters"
                    )
                if body.get("pairingCredential") != secret:
                    raise AssertionError("callback Pairing Credential changed")
                response = {}
                evidence = {
                    "request": "callback",
                    "machineId": machine_id,
                    "pairingCredential": "matched",
                }
            with open(evidence_file, "a", encoding="utf-8") as evidence_stream:
                evidence_stream.write(json.dumps(evidence, sort_keys=True) + "\n")
            payload = json.dumps(response, separators=(",", ":")).encode()
            self.send_response(200)
        except Exception as error:
            Handler.failure = str(error)
            payload = json.dumps({"error": str(error)}).encode()
            self.send_response(400)
        Handler.requests += 1
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_):
        pass


server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
server.timeout = 30
with open(port_file, "w", encoding="ascii") as output:
    output.write(str(server.server_port))
for _ in expected_paths:
    server.handle_request()
server.server_close()
if Handler.requests != len(expected_paths):
    raise SystemExit(
        f"Cloud enrollment fixture received {Handler.requests} of {len(expected_paths)} requests"
    )
if Handler.failure is not None:
    raise SystemExit(Handler.failure)
