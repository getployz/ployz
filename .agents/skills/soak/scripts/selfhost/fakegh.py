# The fake github.com and api.github.com of the self-host lab (compose.override.yml): OAuth turns any code into
# the token of the login it names, and /user answers as that login. TLS uses the certificate setup writes to /fake.
import json, ssl, sys, hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs
USERS = {}
def user(login):
    uid = int(hashlib.sha1(login.encode()).hexdigest()[:7], 16)
    return {"id": uid, "login": login, "name": login.capitalize() + " Soak", "email": f"{login}@soak.test", "avatar_url": ""}
class H(BaseHTTPRequestHandler):
    def _send(self, code, body):
        b = json.dumps(body).encode()
        self.send_response(code); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def _login(self):
        a = self.headers.get("authorization", "")
        return a.split("gho_fake_")[-1] if "gho_fake_" in a else None
    def do_POST(self):
        n = int(self.headers.get("content-length", 0)); raw = self.rfile.read(n).decode()
        sys.stderr.write(f"POST {self.headers.get('host')}{self.path}\n")
        if self.path.startswith("/login/oauth/access_token"):
            q = parse_qs(raw) if "=" in raw and not raw.startswith("{") else json.loads(raw or "{}")
            code = q.get("code"); code = code[0] if isinstance(code, list) else code
            return self._send(200, {"access_token": f"gho_fake_{code}", "token_type": "bearer", "scope": "read:user,user:email"})
        self._send(404, {"message": "Not Found"})
    def do_GET(self):
        sys.stderr.write(f"GET {self.headers.get('host')}{self.path}\n")
        login = self._login(); p = urlparse(self.path).path
        if login and p == "/user": return self._send(200, user(login))
        if login and p == "/user/emails": return self._send(200, [{"email": f"{login}@soak.test", "primary": True, "verified": True, "visibility": "public"}])
        if login and p == "/user/installations": return self._send(200, {"total_count": 0, "installations": []})
        self._send(404, {"message": "Not Found"})
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); ctx.load_cert_chain("/fake/srv.pem", "/fake/srv.key")
s = ThreadingHTTPServer(("0.0.0.0", 443), H); s.socket = ctx.wrap_socket(s.socket, server_side=True); s.serve_forever()
