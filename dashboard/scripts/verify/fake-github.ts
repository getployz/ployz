// The fake GitHub a FAKE_GITHUB=1 verify lab points every GitHub URL at: the App's API, its install and OAuth pages,
// tarballs, and a control route that signs and posts webhooks to Cloud. up.sh runs it with runner.mjs until down.sh
// stops it. Prints `VERIFY_FAKE_GITHUB <url>` once listening, then one line per request.
import { createHash, createHmac, generateKeyPairSync, randomBytes, randomUUID, sign } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { dirname } from "node:path";
import { gzipSync } from "node:zlib";

type User = { id: number; login: string; name: string; email: string };
type Repository = {
  id: number; name: string; private: boolean; default_branch: string; branches: string[];
  awaitsGrant?: boolean;
  files: Record<string, string>;
  pullRequests?: PullRequestSeed[];
};
type PullRequest = {
  number: number; title: string; author: string; head_branch: string; head_sha: string; base_branch: string;
  state: "open" | "closed"; merged: boolean; merge_commit_sha: string | null; commits: number; updated_at: string;
};
type PullRequestSeed = Pick<PullRequest, "number" | "head_branch"> & Partial<PullRequest>;
type WebhookInput = {
  event?: string; action?: string; installation?: number; sender?: string; repositories?: string[];
  repository?: string; number?: number; head_branch?: string; target_branch?: string; merged?: boolean;
  ref?: string; head_sha?: string; conclusion?: string; run_id?: number;
};
type Run = { repository: string; status: "in_progress" | "completed"; conclusion: string | null };
/** `users` reach the installation; on an Organization only `admins` own it, the rest collaborate on a repository. */
type Installation = {
  id: number;
  account: { id: number; login: string; type: "User" | "Organization" };
  users: string[];
  admins?: string[];
  repositories: Repository[];
};
type Fixture = {
  app: { slug: string; afterInstall: { callbackPath: string | null } };
  users: User[];
  installations: Installation[];
};

const FIXTURE_PATH = ".verify/github.json";
const BUILD_WORKFLOW = ".github/workflows/ployz-build.yml";
const GITHUB_ACTIONS_APP = { id: 15368, slug: "github-actions" };
const SUSPENDED = { status: 403, json: { message: "This installation has been suspended" } } as const;
const site = {
  "Dockerfile": "FROM nginx:1.27-alpine\nCOPY index.html /usr/share/nginx/html/index.html\n",
  "index.html": "<!doctype html><title>fake</title><h1>Hello from the fake GitHub</h1>\n",
};
const repository = (id: number, name: string, extra: Partial<Repository> = {}): Repository =>
  ({ id, name, private: true, default_branch: "main", branches: ["main", "dev"], files: site, ...extra });
const DEFAULT_FIXTURE: Fixture = {
  app: { slug: "ployz-verify", afterInstall: { callbackPath: "/api/github/callback" } },
  users: [
    { id: 9001, login: "ada", name: "Ada Lovelace", email: "ada@example.com" },
    { id: 9002, login: "grace", name: "Grace Hopper", email: "grace@example.com" },
    { id: 9003, login: "bob", name: "Bob Bobson", email: "bob@example.com" },
    { id: 9004, login: "mallory", name: "Mallory Outside", email: "mallory@example.com" },
  ],
  installations: [
    { id: 7, account: { id: 5001, login: "acme", type: "Organization" }, users: ["ada", "grace", "mallory"], admins: ["ada", "grace"], repositories: [
      repository(4242, "blog", { private: false }), repository(4243, "web"), repository(4244, "docs", { awaitsGrant: true }),
    ] },
    { id: 8, account: { id: 9001, login: "ada", type: "User" }, users: ["ada"], repositories: [repository(4250, "notes")] },
    { id: 9, account: { id: 5003, login: "hopper-labs", type: "Organization" }, users: ["grace"], admins: ["grace"], repositories: [repository(4260, "compiler")] },
    { id: 10, account: { id: 9003, login: "bob", type: "User" }, users: ["bob"], repositories: [repository(4270, "shop")] },
  ],
};

/**
 * `edited` with every default it lacks: missing keys, and entries of an id-keyed list whose id it doesn't have.
 * What it has wins, so edits survive; an entry deleted from a default list comes back.
 */
function withDefaults(edited: unknown, defaults: unknown): unknown {
  const idOf = (entry: unknown) => (typeof entry === "object" && entry !== null && "id" in entry ? entry.id : undefined);
  if (Array.isArray(edited) && Array.isArray(defaults) && defaults.every((entry) => idOf(entry) !== undefined)) {
    const merged = edited.map((entry) => withDefaults(entry, defaults.find((d) => idOf(d) === idOf(entry))));
    return [...merged, ...defaults.filter((d) => !edited.some((entry) => idOf(entry) === idOf(d)))];
  }
  if (typeof edited !== "object" || edited === null || Array.isArray(edited)) return edited ?? defaults;
  if (typeof defaults !== "object" || defaults === null || Array.isArray(defaults)) return edited;
  const merged: Record<string, unknown> = { ...defaults };
  for (const [key, value] of Object.entries(edited)) merged[key] = withDefaults(value, (defaults as Record<string, unknown>)[key]);
  return merged;
}

function loadFixture(): Fixture {
  const fixture = existsSync(FIXTURE_PATH)
    ? withDefaults(JSON.parse(readFileSync(FIXTURE_PATH, "utf8")), DEFAULT_FIXTURE) as Fixture
    : DEFAULT_FIXTURE;
  mkdirSync(dirname(FIXTURE_PATH), { recursive: true });
  writeFileSync(FIXTURE_PATH, `${JSON.stringify(fixture, null, 2)}\n`);
  return fixture;
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
  let checkSuiteId = 1;
  let runId = 30001;
  const suspended = new Map<number, User>();
  const pulls = new Map<string, PullRequest>();
  const checkSuites = new Map<number, { repositoryId: number; suite: object }>();
  const runs = new Map<number, Run>();
  const signingKey = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const jwk = { ...signingKey.publicKey.export({ format: "jwk" }), kid: "verify-1", alg: "RS256", use: "sig" };

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

  const pullKey = (r: Repository, number: number) => `${r.id}#${number}`;
  const pullRequest = (i: Installation, r: Repository, seed: PullRequestSeed): PullRequest => ({
    title: `Pull request #${seed.number}`, author: i.users[0] ?? "ghost", head_sha: commitSha(i, r, seed.head_branch),
    base_branch: r.default_branch, state: "open", merged: false, merge_commit_sha: null, commits: 1,
    updated_at: "2026-10-01T00:00:00Z", ...seed,
  });
  for (const { installation: i, repository: r } of located) {
    for (const seed of r.pullRequests ?? []) pulls.set(pullKey(r, seed.number), pullRequest(i, r, seed));
  }

  const repositoryJson = (i: Installation, r: Repository) => ({
    id: r.id, node_id: `R_${r.id}`, name: r.name, full_name: fullName(i, r), private: r.private,
    owner: { login: i.account.login, type: i.account.type }, default_branch: r.default_branch,
    html_url: `${origin}/${fullName(i, r)}`, updated_at: "2026-10-01T00:00:00Z",
  });
  const installationJson = (i: Installation) => ({
    id: i.id, app_slug: fixture.app.slug, target_type: i.account.type,
    account: { id: i.account.id, login: i.account.login, type: i.account.type, avatar_url: `${origin}/${i.account.login}.png` },
    html_url: i.account.type === "Organization"
      ? `${origin}/organizations/${i.account.login}/settings/installations/${i.id}`
      : `${origin}/settings/installations/${i.id}`,
    permissions: { metadata: "read", contents: "read", pull_requests: "read", checks: "write", actions: "write" },
    repository_selection: "selected",
    ...suspension(i),
  });
  const suspension = (i: Installation) => {
    const by = suspended.get(i.id);
    return by === undefined
      ? { suspended_at: null, suspended_by: null }
      : { suspended_at: "2026-10-01T00:00:00Z", suspended_by: { id: by.id, login: by.login, type: "User" } };
  };
  const pullJson = (i: Installation, r: Repository, p: PullRequest) => ({
    number: p.number, state: p.state, title: p.title, user: { id: user(p.author)?.id ?? 1, login: p.author, type: "User" },
    head: { ref: p.head_branch, sha: p.head_sha, repo: repositoryJson(i, r) },
    base: { ref: p.base_branch, sha: commitSha(i, r, p.base_branch), repo: repositoryJson(i, r) },
    merged: p.merged, merge_commit_sha: p.merge_commit_sha, commits: p.commits, updated_at: p.updated_at,
    html_url: `${origin}/${fullName(i, r)}/pull/${p.number}`,
  });

  type Caller = { installationId?: number; user?: User };
  const caller = (request: IncomingMessage): Caller => {
    const bearer = /^(?:Bearer|token) (.+)$/.exec(request.headers.authorization ?? "")?.[1] ?? "";
    return { installationId: installationTokens.get(bearer), user: userTokens.get(bearer) };
  };
  const granted = (i: Installation, r: Repository) => installation(i.id) !== undefined && !hidden.has(r.id);
  const readable = (who: Caller, i: Installation, r: Repository) =>
    !r.private || (granted(i, r)
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

  async function sendWebhook(input: WebhookInput) {
    const target = fixture.installations.find((i) => i.id === input.installation);
    const from = user(input.sender) ?? user(target?.users[0]);
    if (!target || !from) return { status: 400, body: "Unknown installation or sender." };
    if (input.event === "installation") {
      if (input.action === "deleted") removedInstallations.add(target.id);
      if (input.action === "created") removedInstallations.delete(target.id);
      if (input.action === "suspend") suspended.set(target.id, from);
      if (input.action === "unsuspend") suspended.delete(target.id);
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
    if (input.event === "pull_request" || input.event === "push" || input.event === "check_suite" || input.event === "workflow_run") {
      const r = target.repositories.find((candidate) => fullName(target, candidate) === input.repository);
      if (!r) return { status: 400, body: "Unknown repository for this installation." };
      return repositoryWebhook(input, target, r, from);
    }
    return { status: 400, body: "event must be installation, installation_repositories, pull_request, push, check_suite or workflow_run." };
  }

  async function repositoryWebhook(input: WebhookInput, i: Installation, r: Repository, from: User) {
    const common = { installation: { id: i.id }, repository: repositoryJson(i, r), sender: sender(from) };
    const now = new Date().toISOString();
    if (input.event === "push") {
      const branch = /^refs\/heads\/(.+)$/.exec(input.ref ?? "")?.[1];
      if (branch === undefined) return { status: 400, body: "ref must be refs/heads/<branch>." };
      const after = commitSha(i, r, branch);
      return webhook("push", {
        ...common, ref: input.ref, before: sha1("before", after), after, created: false, deleted: false, forced: false,
        pusher: { name: from.login, email: from.email },
      });
    }
    if (input.event === "workflow_run") {
      const id = Number(input.run_id);
      const run = runs.get(id);
      if (!run || run.repository !== fullName(i, r)) return { status: 400, body: "Unknown run for this repository." };
      if (input.action === "completed") Object.assign(run, { status: "completed", conclusion: input.conclusion ?? "success" });
      return webhook("workflow_run", {
        ...common, action: input.action, workflow_run: { id, path: BUILD_WORKFLOW, status: run.status, conclusion: run.conclusion },
      });
    }
    if (input.event === "check_suite") {
      const completed = input.action === "completed";
      const suite = {
        id: checkSuiteId++, head_branch: r.default_branch, head_sha: input.head_sha ?? commitSha(i, r, r.default_branch),
        status: completed ? "completed" : "queued", conclusion: completed ? input.conclusion ?? "success" : null,
        updated_at: now, app: GITHUB_ACTIONS_APP,
      };
      checkSuites.set(suite.id, { repositoryId: r.id, suite });
      return webhook("check_suite", { ...common, action: input.action, check_suite: suite });
    }
    if (typeof input.number !== "number") return { status: 400, body: "number is required." };
    const before = pulls.get(pullKey(r, input.number))
      ?? pullRequest(i, r, { number: input.number, author: from.login, head_branch: input.head_branch ?? `pr-${input.number}` });
    const headBranch = input.head_branch ?? before.head_branch;
    const p: PullRequest = {
      ...before, head_branch: headBranch, base_branch: input.target_branch ?? before.base_branch, updated_at: now,
      head_sha: headBranch === before.head_branch ? before.head_sha : commitSha(i, r, headBranch),
    };
    if (input.action === "synchronize") Object.assign(p, { head_sha: sha1("synchronize", p.head_sha), commits: p.commits + 1 });
    if (input.action === "opened" || input.action === "reopened") p.state = "open";
    if (input.action === "closed") {
      const merged = input.merged ?? false;
      Object.assign(p, { state: "closed", merged, merge_commit_sha: merged ? sha1("merge", p.head_sha) : null });
    }
    pulls.set(pullKey(r, p.number), p);
    const changes = p.base_branch === before.base_branch
      ? {}
      : { base: { ref: { from: before.base_branch }, sha: { from: commitSha(i, r, before.base_branch) } } };
    return webhook("pull_request", {
      ...common, action: input.action, number: p.number, pull_request: pullJson(i, r, p),
      ...(input.action === "edited" ? { changes } : {}),
    });
  }

  type TokenInput = {
    aud?: string; repository_id?: number | string; run_id?: number | string; job_workflow_ref?: string;
    event_name?: string; expiresInSeconds?: number;
  };
  function idToken(input: TokenInput) {
    const now = Math.floor(Date.now() / 1000);
    const part = (value: object) => Buffer.from(JSON.stringify(value)).toString("base64url");
    const unsigned = `${part({ alg: "RS256", typ: "JWT", kid: jwk.kid })}.${part({
      iss: origin, aud: input.aud, iat: now, nbf: now, exp: now + (input.expiresInSeconds ?? 300),
      repository_id: String(input.repository_id), run_id: String(input.run_id), job_workflow_ref: input.job_workflow_ref,
      event_name: input.event_name ?? "workflow_dispatch",
    })}`;
    return `${unsigned}.${sign("RSA-SHA256", Buffer.from(unsigned), signingKey.privateKey).toString("base64url")}`;
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
  const pullReply = (i: Installation, r: Repository, number: number): Reply => {
    const p = pulls.get(pullKey(r, number));
    return p ? { status: 200, json: pullJson(i, r, p) } : notFound;
  };

  async function route(request: IncomingMessage, body: string): Promise<Reply> {
    const url = new URL(request.url ?? "/", origin);
    const method = request.method ?? "GET";
    const segments = url.pathname.split("/").slice(1).map(decodeURIComponent);
    const path = `/${segments.join("/")}`;
    const who = caller(request);
    let m: RegExpExecArray | null;
    if (who.installationId !== undefined && suspended.has(who.installationId)) return SUSPENDED;

    if (method === "POST" && path === "/_fake/webhook") {
      const result = await sendWebhook(JSON.parse(body || "{}"));
      return { status: result.status, body: result.body, type: "text/plain" };
    }
    if (method === "POST" && path === "/_fake/oidc") {
      const input = JSON.parse(body || "{}") as TokenInput;
      if (input.aud === undefined || input.job_workflow_ref === undefined || input.repository_id === undefined || input.run_id === undefined) {
        return { status: 400, body: "aud, repository_id, run_id and job_workflow_ref are required.", type: "text/plain" };
      }
      return { status: 200, json: { token: idToken(input) } };
    }
    if (method === "POST" && path === "/_fake/run") {
      const input = JSON.parse(body || "{}") as { repository?: string; run_id?: number; status?: string };
      const target = runs.get(Number(input.run_id));
      if (!target || target.repository !== input.repository || (input.status !== "completed" && input.status !== "in_progress")) {
        return { status: 400, body: "Unknown run for this repository, or status is not completed or in_progress.", type: "text/plain" };
      }
      Object.assign(target, { status: input.status, conclusion: input.status === "completed" ? "success" : null });
      return { status: 200, json: { id: Number(input.run_id), ...target } };
    }
    if (path === "/.well-known/jwks") return { status: 200, json: { keys: [jwk] } };
    if (path === "/.well-known/openid-configuration") {
      return { status: 200, json: { issuer: origin, jwks_uri: `${origin}/.well-known/jwks`, id_token_signing_alg_values_supported: ["RS256"] } };
    }

    if ((m = /^\/app\/installations\/(\d+)(\/access_tokens)?$/.exec(path))) {
      const i = installation(Number(m[1]));
      if (!i) return notFound;
      if (!m[2]) return { status: 200, json: installationJson(i) };
      if (suspended.has(i.id)) return SUSPENDED;
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
      if (path === "/user/installations") {
        const perPage = Number(url.searchParams.get("per_page") ?? 30);
        const page = Number(url.searchParams.get("page") ?? 1);
        const slice = mine.slice((page - 1) * perPage, page * perPage);
        return { status: 200, json: { total_count: mine.length, installations: slice.map(installationJson) } };
      }
      if ((m = /^\/user\/memberships\/orgs\/([^/]+)$/.exec(path))) {
        const org = m[1] ?? "";
        const owns = fixture.installations.some((i) => i.account.type === "Organization" && i.account.login === org && i.admins?.includes(u.login));
        return owns ? { status: 200, json: { state: "active", role: "admin", organization: { login: org } } } : notFound;
      }
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
      const setupAction = url.searchParams.get("setup_action") === "update" ? "update" : "install";
      const deliver = () => sendWebhook({ event: "installation", action: "created", installation: id, sender: from.login });
      let delivered: { status: number } | undefined;
      if (setupAction === "install" && url.searchParams.get("order") === "callback-first") {
        setTimeout(() => void deliver().catch((error: unknown) => console.log(`${new Date().toISOString()} deferred installation webhook failed: ${String(error)}`)), 700);
      }
      else if (setupAction === "install") delivered = await deliver();
      const { callbackPath } = fixture.app.afterInstall;
      if (callbackPath === null) {
        return { status: 200, html: page(`Installed on ${target.account.login} (Cloud answered ${delivered?.status ?? "nothing yet"})`, [], "setTimeout(() => window.close(), 800)") };
      }
      const code = token("code");
      codes.set(code, from);
      const back = new URL(callbackPath, cloud);
      back.searchParams.set("code", code);
      back.searchParams.set("installation_id", String(id));
      back.searchParams.set("setup_action", setupAction);
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
      const pull = /^\/pulls\/(\d+)$/.exec(rest);
      if (pull) return pullReply(found.installation, found.repository, Number(pull[1]));
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
      if ((n = /^\/pulls\/(\d+)$/.exec(rest))) return pullReply(i, r, Number(n[1]));
      if ((n = /^\/check-suites\/(\d+)$/.exec(rest))) {
        const recorded = checkSuites.get(Number(n[1]));
        return recorded?.repositoryId === r.id ? { status: 200, json: recorded.suite } : notFound;
      }
      if ((n = /^\/actions\/workflows\/ployz-build\.yml(\/dispatches)?$/.exec(rest))) {
        if (r.files[BUILD_WORKFLOW] === undefined) return notFound;
        if (!n[1]) return { status: 200, json: { id: r.id, name: "Ployz build", path: BUILD_WORKFLOW, state: "active" } };
        if (method !== "POST") return notFound;
        const id = runId++;
        runs.set(id, { repository: fullName(i, r), status: "in_progress", conclusion: null });
        return { status: 200, json: { workflow_run_id: id, run_url: `${origin}/repos/${fullName(i, r)}/actions/runs/${id}`, html_url: `${origin}/${fullName(i, r)}/actions/runs/${id}` } };
      }
      if ((n = /^\/actions\/runs\/(\d+)(\/cancel)?$/.exec(rest))) {
        const id = Number(n[1]);
        const recorded = runs.get(id);
        if (recorded?.repository !== fullName(i, r)) return notFound;
        if (!n[2]) return { status: 200, json: { id, path: BUILD_WORKFLOW, ...recorded } };
        if (method !== "POST") return notFound;
        if (recorded.status === "completed") return { status: 409, json: { message: "Cannot cancel a workflow run that is completed." } };
        Object.assign(recorded, { status: "completed", conclusion: "cancelled" });
        return { status: 202, json: {} };
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
        i.account.login === m?.[1] && r.name === m[2] && (!r.private || granted(i, r)) && shaOrBranchHead(i, r, sha));
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
