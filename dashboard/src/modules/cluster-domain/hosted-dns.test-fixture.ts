import { execFile as execFileCallback } from "node:child_process";
import { X509Certificate } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

const execFile = promisify(execFileCallback);

export type HostedDnsRequest = { method: string; path: string; authorization: string | null; body: unknown };

/**
 * An in-process Hosted DNS: grants `<preferred>.ployz.test` (`<preferred>-x7k2.ployz.test` when that name is
 * retired), answers a lease renewal with 200 and every other call with 204. `failWith` fails every call;
 * `gone` answers calls for a name with a status (404 reaped, 410 retired) until the name is granted again. The certificate route signs the CSR with a throwaway
 * `openssl` CA for `certificateDays`, refuses (422) one naming anything but `name` and `*.name`, and
 * answers `certificateFailWith` when set.
 */
export async function startFakeHostedDns() {
  const requests: HostedDnsRequest[] = [];
  const state = {
    failWith: null as number | null,
    gone: new Map<string, number>(),
    certificateFailWith: null as number | null,
    certificateDays: 90,
  };
  const ca = await mkdtemp(join(tmpdir(), "fake-hosted-dns-"));
  await execFile("openssl", ["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
    "-subj", "/CN=Fake Hosted DNS CA", "-days", "1", "-keyout", join(ca, "ca.key"), "-out", join(ca, "ca.pem")]);
  const issue = async (name: string, csr: string) => {
    await writeFile(join(ca, "request.csr"), csr);
    const { stdout: leaf } = await execFile("openssl", ["x509", "-req", "-in", join(ca, "request.csr"), "-CA", join(ca, "ca.pem"),
      "-CAkey", join(ca, "ca.key"), "-days", String(state.certificateDays), "-copy_extensions", "copy"]);
    const certificate = new X509Certificate(leaf);
    // Node reports an empty subject as undefined, despite its type.
    return !certificate.subject && certificate.subjectAltName === `DNS:${name}, DNS:*.${name}` ? leaf : null;
  };
  const server = createServer((request, response) => {
    let text = "";
    request.on("data", (chunk: Buffer) => { text += chunk.toString("utf8"); });
    request.on("end", () => {
      const body = text === "" ? null : JSON.parse(text) as { preferred?: string };
      const path = request.url ?? "";
      requests.push({ method: request.method ?? "", path, authorization: request.headers.authorization ?? null, body });
      const goneStatus = state.gone.get(decodeURIComponent(path.split("/")[2] ?? ""));
      if (state.failWith !== null) {
        response.writeHead(state.failWith).end();
      } else if (request.method === "POST" && path === "/domains") {
        const preferred = `${body?.preferred}.ployz.test`;
        // A retired name is never granted again; a reaped one is.
        const name = state.gone.get(preferred) === 410 ? `${body?.preferred}-x7k2.ployz.test` : preferred;
        state.gone.delete(name);
        response.writeHead(201, { "content-type": "application/json" })
          .end(JSON.stringify({ name, token: `token-${requests.length}` }));
      } else if (goneStatus !== undefined) {
        response.writeHead(goneStatus).end();
      } else if (path.endsWith("/certificate")) {
        const name = decodeURIComponent(path.split("/")[2] ?? "");
        if (state.certificateFailWith !== null) {
          response.writeHead(state.certificateFailWith, { "retry-after": "3600" }).end();
          return;
        }
        void issue(name, (body as { csr: string }).csr).then(
          (leaf) => leaf === null
            ? response.writeHead(422).end()
            : response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ certificate_chain_pem: leaf })),
          () => response.writeHead(422).end(),
        );
      } else if (path.endsWith("/lease")) {
        response.writeHead(200, { "content-type": "application/json" })
          .end(JSON.stringify({ name: path.split("/")[2], lease_expires_at: new Date(Date.now() + 7 * 86_400_000).toISOString() }));
      } else {
        response.writeHead(204).end();
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}/`,
    requests,
    state,
    close: async () => {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      await rm(ca, { recursive: true, force: true });
    },
  };
}

export type FakeHostedDns = Awaited<ReturnType<typeof startFakeHostedDns>>;
