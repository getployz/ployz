#!/usr/bin/env python3
"""Sits on Corrosion's admin socket and reports every actor at HOLDER's address as state
"Left", a membership state this daemon does not recognize (as from a newer Corrosion), so
`server ls` from this Entry shows the holder as Left and it gets no RPC.
Usage: leftproxy.py SOCKET HOLDER_IPV6... The real socket moves to SOCKET.real; undo with
unproxy.sh. HOLDER_IPV6 is the host part of `[gossip] addr` in the holder's
/var/lib/ployz/corrosion/config.toml."""
import ipaddress, json, os, socket, struct, sys, threading

path, holders = sys.argv[1], {ipaddress.ip_address(a) for a in sys.argv[2:]}
real = path + ".real"
if not os.path.exists(real):
    os.rename(path, real)

def frames(sock):
    while True:
        head = recv(sock, 4)
        if head is None:
            return
        body = recv(sock, struct.unpack(">I", head)[0])
        if body is None:
            return
        yield body

def recv(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            return None
        data += chunk
    return data

def at_holder(addr):
    try:
        return ipaddress.ip_address(addr.rsplit(":", 1)[0].strip("[]")) in holders
    except ValueError:
        return False

def rewrite(value):
    if isinstance(value, dict):
        if isinstance(value.get("state"), str) and at_holder(str(value.get("id", {}).get("addr", ""))):
            print("rewrote", value["id"]["addr"], value["state"], "-> Left", flush=True)
            value["state"] = "Left"
        for item in value.values():
            rewrite(item)
    elif isinstance(value, list):
        for item in value:
            rewrite(item)

def pipe(src, dst, edit):
    try:
        for body in frames(src):
            if edit:
                try:
                    parsed = json.loads(body)
                    rewrite(parsed)
                    body = json.dumps(parsed).encode()
                except ValueError:
                    pass
            dst.sendall(struct.pack(">I", len(body)) + body)
    finally:
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

def serve(client):
    upstream = socket.socket(socket.AF_UNIX)
    upstream.connect(real)
    threading.Thread(target=pipe, args=(client, upstream, False), daemon=True).start()
    pipe(upstream, client, True)

server = socket.socket(socket.AF_UNIX)
if os.path.exists(path):
    os.unlink(path)
server.bind(path)
os.chmod(path, 0o777)
server.listen(64)
while True:
    conn, _ = server.accept()
    threading.Thread(target=serve, args=(conn,), daemon=True).start()
