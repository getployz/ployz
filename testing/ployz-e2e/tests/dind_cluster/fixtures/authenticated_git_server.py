import base64
import http.server
import os
import ssl
import subprocess
import urllib.parse


git_root = os.environ.get("GIT_ROOT", "/tmp/ployz-build-git")
auth_log = os.environ.get("GIT_AUTH_LOG", os.path.join(git_root, "auth.log"))
expected = "Basic " + base64.b64encode(
    (os.environ["GIT_USERNAME"] + ":" + os.environ["GIT_PASSWORD"]).encode()
).decode()
git_http_backend = os.path.join(
    subprocess.check_output(["git", "--exec-path"], text=True).strip(),
    "git-http-backend",
)


class AuthenticatedSmartGit(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self.serve_git()

    def do_POST(self):
        self.serve_git()

    def serve_git(self):
        if self.headers.get("Authorization") != expected:
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="ployz-build"')
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        parsed = urllib.parse.urlsplit(self.path)
        if not parsed.path.startswith("/repo.git/"):
            self.send_error(404)
            return
        with open(auth_log, "a", encoding="utf-8") as log:
            log.write("authorized\n")

        content_length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(content_length)
        environment = os.environ.copy()
        environment.update(
            {
                "GIT_PROJECT_ROOT": git_root,
                "GIT_HTTP_EXPORT_ALL": "1",
                "PATH_INFO": parsed.path,
                "QUERY_STRING": parsed.query,
                "REQUEST_METHOD": self.command,
                "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                "CONTENT_LENGTH": str(content_length),
                "REMOTE_USER": os.environ["GIT_USERNAME"],
                "SERVER_NAME": self.server.server_address[0],
                "SERVER_PORT": str(self.server.server_address[1]),
                "SERVER_PROTOCOL": self.request_version,
            }
        )
        backend = subprocess.run(
            [git_http_backend],
            input=body,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            timeout=30,
            check=False,
        )
        if backend.returncode != 0:
            self.send_error(500)
            return

        headers, separator, response = backend.stdout.partition(b"\r\n\r\n")
        if not separator:
            headers, separator, response = backend.stdout.partition(b"\n\n")
        if not separator:
            self.send_error(500)
            return
        status = 200
        outgoing = []
        for line in headers.decode("iso-8859-1").splitlines():
            name, value = line.split(":", 1)
            if name.lower() == "status":
                status = int(value.strip().split(" ", 1)[0])
            else:
                outgoing.append((name, value.strip()))
        self.send_response(status)
        for name, value in outgoing:
            self.send_header(name, value)
        if not any(name.lower() == "content-length" for name, _ in outgoing):
            self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, format, *args):
        return


port = int(os.environ.get("GIT_PORT", "9443"))
httpd = http.server.ThreadingHTTPServer(("0.0.0.0", port), AuthenticatedSmartGit)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(
    os.environ.get("GIT_CERT", os.path.join(git_root, "server.crt")),
    os.environ.get("GIT_KEY", os.path.join(git_root, "server.key")),
)
httpd.socket = context.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
