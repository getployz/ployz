#!/usr/bin/env python3
"""A fake legacy Docker volume plugin at /run/docker/plugins/hang.sock. While /run/hangplug-hang
exists, its VolumeDriver.List hangs for an hour, so `docker volume ls` on this Machine stalls
(about 15 s, Docker's own timeout) while `docker ps` answers at once. Every other call answers
as an empty local driver.
Usage: hangplug.py, then touch /run/hangplug-hang to arm and remove it to disarm."""
import json, os, socketserver, time
from http.server import BaseHTTPRequestHandler

SOCK = "/run/docker/plugins/hang.sock"
ARMED = "/run/hangplug-hang"


class H(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if self.path == "/VolumeDriver.List" and os.path.exists(ARMED):
            time.sleep(3600)
        body = {"/Plugin.Activate": {"Implements": ["VolumeDriver"]},
                "/VolumeDriver.List": {"Volumes": []},
                "/VolumeDriver.Capabilities": {"Capabilities": {"Scope": "local"}}}.get(self.path, {"Err": "not supported"})
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/vnd.docker.plugins.v1+json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class S(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


os.makedirs("/run/docker/plugins", exist_ok=True)
if os.path.exists(SOCK):
    os.unlink(SOCK)
S(SOCK, H).serve_forever()
