#!/usr/bin/env python3
"""Sits on the ployz Docker volume plugin socket and answers one kind of request with an Err at
once; every other request passes through to the real plugin.
Usage: plugproxy.py list         fails VolumeDriver.List: Docker warns `list ployz: …`, and ployzd
                                 reads every Managed Volume on this Server as failed.
       plugproxy.py get NEEDLE   fails VolumeDriver.Get for Volumes whose request names NEEDLE.
                                 Docker then drops that Volume's labels, which a later deploy
                                 refuses, so use it on a Volume you can discard.
The real socket moves to ployz.sock.real; undo with unproxy.sh. Docker keeps a keep-alive
connection to the plugin, so restart ployz-volume-plugin.service after starting this."""
import os, socket, sys, threading

PATH = "/run/docker/plugins/ployz.sock"
REAL = PATH + ".real"
mode = sys.argv[1] if len(sys.argv) > 1 else ""
if mode == "get" and len(sys.argv) == 3:
    needle = sys.argv[2].encode()
elif mode != "list" or len(sys.argv) != 2:
    sys.exit("usage: plugproxy.py list | plugproxy.py get NEEDLE")
if not os.path.exists(REAL):
    os.rename(PATH, REAL)


def failing(line, body):
    if mode == "list":
        return b"/VolumeDriver.List" in line
    return b"/VolumeDriver.Get" in line and needle in body


def read_request(sock):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(65536)
        if not chunk:
            return None
        data += chunk
    head, _, body = data.partition(b"\r\n\r\n")
    length = 0
    for line in head.split(b"\r\n")[1:]:
        k, _, v = line.partition(b":")
        if k.strip().lower() == b"content-length":
            length = int(v.strip())
    while len(body) < length:
        chunk = sock.recv(65536)
        if not chunk:
            break
        body += chunk
    return head, body


def dechunk(data):
    out = b""
    while data:
        size, _, rest = data.partition(b"\r\n")
        n = int(size.split(b";")[0], 16)
        if n == 0:
            break
        out += rest[:n]
        data = rest[n + 2:]
    return out


def serve(client):
    try:
        while True:
            req = read_request(client)
            if req is None:
                return
            head, body = req
            if failing(head.split(b"\r\n")[0], body):
                print("failed", mode, body[:200], flush=True)
                out = b'{"Err":"injected ' + mode.encode() + b' failure"}'
                client.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.plugins.v1+json\r\n"
                               b"Content-Length: " + str(len(out)).encode() + b"\r\n\r\n" + out)
                continue
            up = socket.socket(socket.AF_UNIX)
            up.connect(REAL)
            # One request per upstream connection, so the response ends at close.
            head = b"\r\n".join(l for l in head.split(b"\r\n") if not l.lower().startswith(b"connection:"))
            up.sendall(head + b"\r\nConnection: close\r\n\r\n" + body)
            resp = b""
            while True:
                chunk = up.recv(65536)
                if not chunk:
                    break
                resp += chunk
            up.close()
            rhead, _, rbody = resp.partition(b"\r\n\r\n")
            lines = [l for l in rhead.split(b"\r\n")
                     if not l.lower().startswith((b"connection:", b"transfer-encoding:", b"content-length:"))]
            if b"chunked" in rhead.lower():
                rbody = dechunk(rbody)
            client.sendall(b"\r\n".join(lines) + b"\r\nContent-Length: " + str(len(rbody)).encode() + b"\r\n\r\n" + rbody)
    except OSError:
        pass
    finally:
        client.close()


server = socket.socket(socket.AF_UNIX)
if os.path.exists(PATH):
    os.unlink(PATH)
server.bind(PATH)
os.chmod(PATH, 0o755)
server.listen(64)
while True:
    conn, _ = server.accept()
    threading.Thread(target=serve, args=(conn,), daemon=True).start()
