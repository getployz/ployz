import { createHash, createHmac, randomBytes, randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { dirname } from "node:path";
import { gzipSync } from "node:zlib";

type User = { id: number; login: string; name: string; email: string };
type Repository = {
  id: number; name: string; private: boolean; default_branch: string; branches: string[];
  awaitsGrant?: true;
  files: Record<string, string>;
};
type Installation = { id: number; account: { login: string; type: "User" | "Organization" }; users: string[]; repositories: Repository[] };
type Fixture = {
  app: { slug: string; afterInstall: { closeWindow: true } | { callbackUrl: string } };
  users: User[];
  installations: Installation[];
};

const FIXTURE_PATH = ".verify/github.json";
const site = {
  "Dockerfile": "FROM nginx:1.27-alpine\nCOPY index.html /usr/share/nginx/html/index.html\n",
  "index.html": "<!doctype html><title>fake</title><h1>Hello from the fake GitHub</h1>\n",
};
const repository = (id: number, name: string, extra: Partial<Repository> = {}): Repository =>
  ({ id, name, private: true, default_branch: "main", branches: ["main", "dev"], files: site, ...extra });
const DEFAULT_FIXTURE: Fixture = {
  app: { slug: "ployz-verify", afterInstall: { closeWindow: true } },
  users: [
    { id: 9001, login: "ada", name: "Ada Lovelace", email: "ada@example.com" },
    { id: 9002, login: "grace", name: "Grace Hopper", email: "grace@example.com" },
  ],
  installations: [
    { id: 7, account: { login: "acme", type: "Organization" }, users: ["ada", "grace"], repositories: [
      repository(4242, "blog", { private: false }), repository(4243, "web"), repository(4244, "docs", { awaitsGrant: true }),
    ] },
    { id: 8, account: { login: "ada", type: "User" }, users: ["ada"], repositories: [repository(4250, "notes")] },
    { id: 9, account: { login: "hopper-labs", type: "Organization" }, users: ["grace"], repositories: [repository(4260, "compiler")] },
  ],
};

function loadFixture(): Fixture {
  if (!existsSync(FIXTURE_PATH)) {
    mkdirSync(dirname(FIXTURE_PATH), { recursive: true });
    writeFileSync(FIXTURE_PATH, `${JSON.stringify(DEFAULT_FIXTURE, null, 2)}\n`);
  }
  return JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as Fixture;
}

const sha1 = (...parts: string[]) => createHash("sha1").update(parts.join("\0")).digest("hex");
const token = (prefix: string) => `${prefix}_${randomBytes(18).toString("hex")}`;

export async function run() {
  const fixture = loadFixture();
  const cloud = process.env["FAKE_GITHUB_CLOUD_URL"] ?? "";
  const webhookSecret = process.env["GITHUB_APP_WEBHOOK_SECRET"] ?? "";
  const removedInstallations = new Set<number>();
  const hidden = new Set(fixture.installations.flatMap((i) => i.repositories.filter((r) => r.awaitsGrant).map((r) => r.id)));
  const installationTokens = new Map<string, number>();
  const userTokens = new Map<string, User>();
  const codes = new Map<string, User>();
  let origin = "";
  let checkRunId = 1;

  const user = (login: string | undefined) => fixture.users.find((u) => u.login === login);
  const installation = (id: number) =>
    removedInstallations.has(id) ? undefined : fixture.installations.find((i) => i.id === id);
  const listed = (i: Installation) => i.repositories.filter((r) => !hidden.has(r.id));
  const fullName = (i: Installation, r: Repository) => `${i.account.login}/${r.name}`;
  const located = fixture.installations.flatMap((i) => i.repositories.map((r) => ({ installation: i, repository: r })));
  const commitSha = (i: Installation, r: Repository, branch: string) => sha1("commit", fullName(i, r), branch, JSON.stringify(r.files));
  const treeSha = (commit: string) => sha1("tree", commit);
  const shaOrBranchHead = (i: Installation, r: Repository, ref: string) => {
    if (/^[0-9a-f]{40}$/.test(ref)) return ref;
    const branch = r.branches.find((b) => b === ref);
    return branch === undefined ? undefined : commitSha(i, r, branch);
  };

  const repositoryJson = (i: Installation, r: Repository) => ({
    id: r.id, node_id: `R_${r.id}`, name: r.name, full_name: fullName(i, r), private: r.private,
    owner: { login: i.account.login, type: i.account.type }, default_branch: r.default_branch,
    html_url: `${origin}/${fullName(i, r)}`, updated_at: "2026-10-01T00:00:00Z",
  });
  const installationJson = (i: Installation) => ({
    id: i.id, app_slug: fixture.app.slug, target_type: i.account.type,
    account: { login: i.account.login, type: i.account.type, avatar_url: `${origin}/${i.account.login}.png` },
    html_url: i.account.type === "Organization"
      ? `${origin}/organizations/${i.account.login}/settings/installations/${i.id}`
      : `${origin}/settings/installations/${i.id}`,
    permissions: { metadata: "read", contents: "read", pull_requests: "read", checks: "write", actions: "write" },
    repository_selection: "selected",
  });

  type Caller = { installationId?: number; user?: User };
  const caller = (request: IncomingMessage): Caller => {
    const bearer = /^(?:Bearer|token) (.+)$/.exec(request.headers.authorization ?? "")?.[1] ?? "";
    return { installationId: installationTokens.get(bearer), user: userTokens.get(bearer) };
  };
  const readable = (who: Caller, i: Installation, r: Repository) =>
    !r.private || (installation(i.id) !== undefined && !hidden.has(r.id)
      && (who.installationId === i.id || (who.user !== undefined && i.users.includes(who.user.login))));
  const byName = (who: Caller, owner: string, name: string) =>
    located.find(({ installation: i, repository: r }) => i.account.login === owner && r.name === name && readable(who, i, r));

  async function webhook(event: string, payload: object) {
    const body = JSON.stringify(payload);
    const signature = `sha256=${createHmac("sha256", webhookSecret).update(body).digest("hex")}`;
    const response = await fetch(`${cloud}/api/github/webhook`, {
      method: "POST", body,
      headers: { "content-type": "application/json", "x-github-event": event, "x-github-delivery": randomUUID(), "x-hub-signature-256": signature },
    });
    return { status: response.status, body: await response.text() };
  }
  const sender = (u: User) => ({ id: u.id, login: u.login, type: "User" });

  async function sendWebhook(input: { event?: string; action?: string; installation?: number; sender?: string; repositories?: string[] }) {
    const target = fixture.installations.find((i) => i.id === input.installation);
    const from = user(input.sender) ?? user(target?.users[0]);
    if (!target || !from) return { status: 400, body: "Unknown installation or sender." };
    if (input.event === "installation") {
      if (input.action === "deleted") removedInstallations.add(target.id);
      if (input.action === "created") removedInstallations.delete(target.id);
      return webhook("installation", {
        action: input.action, sender: sender(from),
        installation: installationJson(target), repositories: listed(target).map((r) => repositoryJson(target, r)),
      });
    }
    if (input.event === "installation_repositories" && (input.action === "added" || input.action === "removed")) {
      const changed = target.repositories.filter((r) => input.repositories?.includes(fullName(target, r)));
      for (const r of changed) {
        if (input.action === "added") hidden.delete(r.id);
        else hidden.add(r.id);
      }
      const repositories = changed.map((r) => repositoryJson(target, r));
      return webhook("installation_repositories", {
        action: input.action, sender: sender(from), installation: installationJson(target), repository_selection: "selected",
        repositories_added: input.action === "added" ? repositories : [], repositories_removed: input.action === "removed" ? repositories : [],
      });
    }
    return { status: 400, body: "event must be installation or installation_repositories." };
  }

  function page(title: string, links: Array<{ href: string; label: string }>, script = "") {
    const items = links.map((l) => `<li><a href="${l.href.replaceAll("&", "&amp;")}">${l.label}</a></li>`).join("");
    return `<!doctype html><meta charset="utf-8"><title>${title}</title><body style="font-family:system-ui;margin:3rem">`
      + `<h1>${title}</h1><ul>${items}</ul><script>${script}</script></body>`;
  }

  function tarball(prefix: string, files: Record<string, string>) {
    const blocks: Buffer[] = [];
    const entry = (name: string, body: Buffer, type: "0" | "5") => {
      const header = Buffer.alloc(512);
      header.write(name, 0, 100);
      header.write(type === "5" ? "0000755\0" : "0000644\0", 100);
      header.write("0000000\0", 108);
      header.write("0000000\0", 116);
      header.write(`${body.length.toString(8).padStart(11, "0")}\0`, 124);
      header.write(`${Math.floor(Date.parse("2026-10-01") / 1000).toString(8).padStart(11, "0")}\0`, 136);
      header.write("        ", 148);
      header.write(type, 156);
      header.write("ustar\0", 257);
      header.write("00", 263);
      header.write(`${header.reduce((sum, byte) => sum + byte, 0).toString(8).padStart(6, "0")}\0 `, 148);
      blocks.push(header, body, Buffer.alloc((512 - (body.length % 512)) % 512));
    };
    const directories = new Set([`${prefix}/`]);
    for (const path of Object.keys(files)) {
      const parts = path.split("/");
      for (let n = 1; n < parts.length; n++) directories.add(`${prefix}/${parts.slice(0, n).join("/")}/`);
    }
    for (const directory of [...directories].sort()) entry(directory, Buffer.alloc(0), "5");
    for (const [path, content] of Object.entries(files)) entry(`${prefix}/${path}`, Buffer.from(content), "0");
    return gzipSync(Buffer.concat([...blocks, Buffer.alloc(1024)]));
  }

  type Reply = { status: number; json?: unknown; html?: string; body?: Buffer | string; type?: string; location?: string };
  const notFound: Reply = { status: 404, json: { message: "Not Found", documentation_url: "https://docs.github.com/rest" } };

  async function route(request: IncomingMessage, body: string): Promise<Reply> {
    const url = new URL(request.url ?? "/", origin);
    const method = request.method ?? "GET";
    const segments = url.pathname.split("/").slice(1).map(decodeURIComponent);
    const path = `/${segments.join("/")}`;
    const who = caller(request);
    let m: RegExpExecArray | null;

    if (method === "POST" && path === "/_fake/webhook") {
      const result = await sendWebhook(JSON.parse(body || "{}"));
      return { status: result.status, body: result.body, type: "text/plain" };
    }

    if ((m = /^\/app\/installations\/(\d+)(\/access_tokens)?$/.exec(path))) {
      const i = installation(Number(m[1]));
      if (!i) return notFound;
      if (!m[2]) return { status: 200, json: installationJson(i) };
      const minted = token("ghs");
      installationTokens.set(minted, i.id);
      return { status: 201, json: { token: minted, expires_at: new Date(Date.now() + 3600_000).toISOString(), permissions: installationJson(i).permissions } };
    }
    if (path === "/installation/repositories") {
      const i = who.installationId === undefined ? undefined : installation(who.installationId);
      if (!i) return { status: 401, json: { message: "Bad credentials" } };
      const perPage = Number(url.searchParams.get("per_page") ?? 30);
      const start = (Number(url.searchParams.get("page") ?? 1) - 1) * perPage;
      const repositories = listed(i);
      return { status: 200, json: { total_count: repositories.length, repositories: repositories.slice(start, start + perPage).map((r) => repositoryJson(i, r)) } };
    }

    if (path === "/login/oauth/authorize") {
      const back = (u: User) => {
        const code = token("code");
        codes.set(code, u);
        const target = new URL(url.searchParams.get("redirect_uri") ?? `${cloud}/`);
        target.searchParams.set("code", code);
        const state = url.searchParams.get("state");
        if (state !== null) target.searchParams.set("state", state);
        return target.toString();
      };
      const chosen = user(url.searchParams.get("login") ?? undefined);
      if (chosen) return { status: 302, location: back(chosen) };
      return { status: 200, html: page("Authorize Ployz", fixture.users.map((u) => {
        const next = new URL(url);
        next.searchParams.set("login", u.login);
        return { href: next.toString(), label: `Continue as ${u.login}` };
      })) };
    }
    if (method === "POST" && path === "/login/oauth/access_token") {
      const form = (request.headers["content-type"] ?? "").includes("json")
        ? new URLSearchParams(JSON.parse(body || "{}")) : new URLSearchParams(body);
      const code = form.get("code") ?? url.searchParams.get("code") ?? "";
      const u = codes.get(code);
      codes.delete(code);
      const answer = u ? (() => {
        const minted = token("ghu");
        userTokens.set(minted, u);
        return { access_token: minted, token_type: "bearer", scope: "" };
      })() : { error: "bad_verification_code", error_description: "The code passed is incorrect or expired." };
      return (request.headers.accept ?? "").includes("json")
        ? { status: 200, json: answer }
        : { status: 200, body: new URLSearchParams(answer).toString(), type: "application/x-www-form-urlencoded" };
    }
    if (path === "/user" || path.startsWith("/user/")) {
      const u = who.user;
      if (!u) return { status: 401, json: { message: "Requires authentication" } };
      const mine = fixture.installations.filter((i) => installation(i.id) && i.users.includes(u.login));
      if (path === "/user") return { status: 200, json: { id: u.id, login: u.login, name: u.name, email: u.email, avatar_url: `${origin}/${u.login}.png`, type: "User" } };
      if (path === "/user/installations") return { status: 200, json: { total_count: mine.length, installations: mine.map(installationJson) } };
      if ((m = /^\/user\/installations\/(\d+)\/repositories$/.exec(path))) {
        const i = mine.find((candidate) => candidate.id === Number(m?.[1]));
        if (!i) return notFound;
        return { status: 200, json: { total_count: listed(i).length, repositories: listed(i).map((r) => repositoryJson(i, r)) } };
      }
      return notFound;
    }

    if ((m = /^\/apps\/([^/]+)\/installations\/new$/.exec(path))) {
      const state = url.searchParams.get("state");
      return { status: 200, html: page(`Install ${m[1]}`, fixture.installations.map((i) => {
        const next = new URL("/_fake/install", origin);
        next.searchParams.set("installation_id", String(i.id));
        if (state !== null) next.searchParams.set("state", state);
        return { href: next.toString(), label: `Install on ${i.account.login} as ${i.users[0]}` };
      })) };
    }
    if (path === "/_fake/install") {
      const id = Number(url.searchParams.get("installation_id"));
      const target = fixture.installations.find((i) => i.id === id);
      const from = user(url.searchParams.get("login") ?? target?.users[0]);
      if (!target || !from) return notFound;
      const delivered = await sendWebhook({ event: "installation", action: "created", installation: id, sender: from.login });
      const after = fixture.app.afterInstall;
      if ("closeWindow" in after) {
        return { status: 200, html: page(`Installed on ${target.account.login} (Cloud answered ${delivered.status})`, [], "setTimeout(() => window.close(), 800)") };
      }
      const code = token("code");
      codes.set(code, from);
      const back = new URL(after.callbackUrl);
      back.searchParams.set("code", code);
      back.searchParams.set("installation_id", String(id));
      back.searchParams.set("setup_action", "install");
      const state = url.searchParams.get("state");
      if (state !== null) back.searchParams.set("state", state);
      return { status: 302, location: back.toString() };
    }

    if ((m = /^\/repositories\/(\d+)(\/.*)?$/.exec(path))) {
      const found = located.find(({ installation: i, repository: r }) => r.id === Number(m?.[1]) && readable(who, i, r));
      if (!found) return notFound;
      const rest = m[2] ?? "";
      if (rest === "") return { status: 200, json: repositoryJson(found.installation, found.repository) };
      if (/^\/commits\/[0-9a-f]{40}\/check-runs$/.test(rest)) return { status: 200, json: { total_count: 0, check_runs: [] } };
      if (/^\/check-runs(\/\d+)?$/.test(rest) && method !== "GET") return { status: 200, json: { id: checkRunId++ } };
      return notFound;
    }
    if ((m = /^\/repos\/([^/]+)\/([^/]+)(\/.*)?$/.exec(path))) {
      const found = byName(who, m[1] ?? "", m[2] ?? "");
      if (!found) return notFound;
      const { installation: i, repository: r } = found;
      const rest = m[3] ?? "";
      let n: RegExpExecArray | null;
      if (rest === "") return { status: 200, json: repositoryJson(i, r) };
      if (rest === "/branches") {
        const branches = url.searchParams.get("page") === "1" || !url.searchParams.has("page") ? r.branches : [];
        return { status: 200, json: branches.map((b) => ({ name: b, commit: { sha: commitSha(i, r, b) }, protected: false })) };
      }
      if ((n = /^\/git\/ref\/heads\/(.+)$/.exec(rest))) {
        const branch = r.branches.find((b) => b === n?.[1]);
        if (!branch) return notFound;
        return { status: 200, json: { ref: `refs/heads/${branch}`, object: { type: "commit", sha: commitSha(i, r, branch) } } };
      }
      if ((n = /^\/commits\/(.+)$/.exec(rest))) {
        const sha = shaOrBranchHead(i, r, n[1] ?? "");
        if (!sha) return notFound;
        return { status: 200, json: { sha, commit: { tree: { sha: treeSha(sha) }, message: "Fake commit" } } };
      }
      if ((n = /^\/git\/trees\/([0-9a-f]{40})$/.exec(rest))) {
        const directories = new Set(Object.keys(r.files).flatMap((p) => p.split("/").slice(0, -1).map((_, k, parts) => parts.slice(0, k + 1).join("/"))));
        const tree = [
          ...[...directories].map((p) => ({ path: p, mode: "040000", type: "tree", sha: sha1("dir", p) })),
          ...Object.entries(r.files).map(([p, content]) => ({ path: p, mode: "100644", type: "blob", sha: sha1("blob", content), size: content.length })),
        ];
        return { status: 200, json: { sha: n[1], truncated: false, tree } };
      }
      if ((n = /^\/contents(?:\/(.*))?$/.exec(rest))) {
        const want = n[1] ?? "";
        if (!shaOrBranchHead(i, r, url.searchParams.get("ref") ?? r.default_branch)) return notFound;
        const content = r.files[want];
        if (content !== undefined) {
          return { status: 200, json: { type: "file", path: want, name: want.split("/").at(-1), size: content.length, encoding: "base64", content: Buffer.from(content).toString("base64") } };
        }
        const prefix = want === "" ? "" : `${want}/`;
        const children = [...new Set(Object.keys(r.files).filter((p) => p.startsWith(prefix)).map((p) => p.slice(prefix.length).split("/")[0] ?? ""))];
        if (children.length === 0) return notFound;
        return { status: 200, json: children.map((c) => ({ path: `${prefix}${c}`, name: c, type: r.files[`${prefix}${c}`] === undefined ? "dir" : "file" })) };
      }
      if ((n = /^\/compare\/([0-9a-f]{40})\.\.\.([0-9a-f]{40})$/.exec(rest))) {
        const identical = n[1] === n[2];
        return { status: 200, json: { status: identical ? "identical" : "ahead", base_commit: { sha: n[1] }, files: identical ? [] : Object.keys(r.files).map((filename) => ({ filename })) } };
      }
      if ((n = /^\/tarball\/([0-9a-f]{40})$/.exec(rest))) {
        if (!shaOrBranchHead(i, r, n[1] ?? "")) return notFound;
        return { status: 302, location: `${origin}/${fullName(i, r)}/legacy.tar.gz/${n[1]}` };
      }
      return notFound;
    }
    if ((m = /^\/([^/]+)\/([^/]+)\/legacy\.tar\.gz\/([0-9a-f]{40})$/.exec(path))) {
      const sha = m[3] ?? "";
      const found = located.find(({ installation: i, repository: r }) =>
        i.account.login === m?.[1] && r.name === m[2] && (!r.private || installation(i.id)) && shaOrBranchHead(i, r, sha));
      if (!found) return notFound;
      const prefix = `${found.installation.account.login}-${found.repository.name}-${sha.slice(0, 7)}`;
      return { status: 200, body: tarball(prefix, found.repository.files), type: "application/x-gzip" };
    }
    return notFound;
  }

  const server = createServer((request, response: ServerResponse) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk: Buffer) => chunks.push(chunk));
    request.on("end", () => {
      void route(request, Buffer.concat(chunks).toString("utf8")).catch((error: unknown): Reply => ({
        status: 500, body: String(error), type: "text/plain",
      })).then((reply) => {
        console.log(`${new Date().toISOString()} ${request.method} ${request.url} ${reply.status}`);
        const headers: Record<string, string> = {};
        if (reply.location) headers["location"] = reply.location;
        if (reply.json !== undefined) {
          response.writeHead(reply.status, { ...headers, "content-type": "application/json; charset=utf-8" });
          response.end(JSON.stringify(reply.json));
        } else if (reply.html !== undefined) {
          response.writeHead(reply.status, { ...headers, "content-type": "text/html; charset=utf-8" });
          response.end(reply.html);
        } else {
          response.writeHead(reply.status, reply.type ? { ...headers, "content-type": reply.type } : headers);
          response.end(reply.body ?? "");
        }
      });
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("fake GitHub has no port");
  origin = `http://127.0.0.1:${address.port}`;
  console.log(`VERIFY_FAKE_GITHUB ${origin}`);
  await new Promise<never>(() => {});
}
