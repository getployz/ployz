import { describe, expect, it } from "vitest";
import { READ_SET } from "./policy.js";
import {
  API,
  APP_ID,
  BUILDER,
  CANONICAL_WORKFLOW,
  CLIENT_SECRET,
  CLOUD,
  CODE,
  HEAD,
  INSTALLATION,
  REPOSITORY,
  USER_TOKEN,
  harness,
  json,
  notFound,
  raw,
  tokenRequests,
  type Call,
  type StubRoutes,
} from "./test/harness.js";

const repo = `/repositories/${REPOSITORY}`;
const WORKFLOW_FILE = `${repo}/contents/.github/workflows/ployz-build.yml`;
const DISPATCH = `${repo}/actions/workflows/ployz-build.yml/dispatches`;

/** Today's dashboard template, byte for byte: the builder by mutable tag. */
const TODAY_WORKFLOW = CANONICAL_WORKFLOW.replace(`getployz/build@${BUILDER}`, "getployz/build@v1");

const sent = (calls: readonly Call[], method: string, path: string) => calls.filter((call) => call.method === method && call.path === path);

describe("/v1/token", () => {
  const refused: [string, unknown][] = [
    ["actions: read", { installation_id: INSTALLATION, permissions: { actions: "read" } }],
    ["contents: write", { installation_id: INSTALLATION, permissions: { contents: "write" } }],
    ["actions: write", { installation_id: INSTALLATION, permissions: { actions: "write" } }],
    ["checks: write", { installation_id: INSTALLATION, permissions: { checks: "write" } }],
    ["administration: read", { installation_id: INSTALLATION, permissions: { administration: "read" } }],
    ["workflows: read", { installation_id: INSTALLATION, permissions: { workflows: "read" } }],
    ["an unknown permission", { installation_id: INSTALLATION, permissions: { not_a_permission: "read" } }],
    ["a level other than read", { installation_id: INSTALLATION, permissions: { contents: "admin" } }],
    ["read mixed with a write", { installation_id: INSTALLATION, permissions: { contents: "read", checks: "write" } }],
    ["empty permissions", { installation_id: INSTALLATION, permissions: {} }],
    ["a non-numeric installation ID", { installation_id: String(INSTALLATION) }],
    ["an extra body field", { installation_id: INSTALLATION, path: "/app/installations/11/access_tokens" }],
    ["more than 100 repositories", { installation_id: INSTALLATION, repository_ids: Array.from({ length: 101 }, (_, i) => i + 1) }],
    ["no repositories in a list", { installation_id: INSTALLATION, repository_ids: [] }],
  ];

  it.each(refused)("refuses %s with 400, an audit line and no token request", async (_, body) => {
    const minter = harness();
    const response = await minter.call("/v1/token", body);
    expect(response.status).toBe(400);
    expect(response.body["error"]).toBe("refused");
    expect(tokenRequests(minter.calls)).toEqual([]);
    expect(minter.audit).toEqual([expect.objectContaining({ route: "/v1/token", outcome: "refused", reason: expect.any(String) })]);
  });

  it("sends the read set explicitly when no permissions are asked", async () => {
    const minter = harness();
    const response = await minter.call("/v1/token", { installation_id: INSTALLATION });
    expect(response.status).toBe(200);
    expect(response.body).toEqual({ token: expect.stringMatching(/^ghs_/), expires_at: "2099-01-01T00:00:00Z" });
    const [request] = tokenRequests(minter.calls);
    expect(request?.path).toBe(`/app/installations/${INSTALLATION}/access_tokens`);
    expect(request?.body).toEqual({ permissions: READ_SET });
    expect(minter.audit[0]).toMatchObject({ outcome: "granted", installation_id: INSTALLATION, token_expires_at: "2099-01-01T00:00:00Z" });
  });

  it("narrows to the repositories and read permissions asked", async () => {
    const minter = harness();
    await minter.call("/v1/token", { installation_id: INSTALLATION, repository_ids: [REPOSITORY], permissions: { contents: "read" } });
    expect(tokenRequests(minter.calls)[0]?.body).toEqual({ repository_ids: [REPOSITORY], permissions: { contents: "read" } });
    expect(minter.audit[0]).toMatchObject({
      repository_ids: [REPOSITORY],
      permissions_asked: { contents: "read" },
      permissions_granted: { metadata: "read", contents: "read" },
    });
  });

  it("returns no token when GitHub grants more than the read set", async () => {
    const minter = harness({
      [`POST /app/installations/${INSTALLATION}/access_tokens`]: () =>
        json({ token: "ghs_wider", expires_at: "2099-01-01T00:00:00Z", permissions: { contents: "write" } }, 201),
    });
    const response = await minter.call("/v1/token", { installation_id: INSTALLATION });
    expect(response).toEqual({ status: 502, body: { error: "github", operation: "installation_token_granted_more", failure: "overbroad" } });
    expect(JSON.stringify(response.body)).not.toContain("ghs_");
  });

  it("signs the token request with an App JWT", async () => {
    const minter = harness();
    await minter.call("/v1/token", { installation_id: INSTALLATION });
    const authorization = tokenRequests(minter.calls)[0]?.headers.get("authorization") ?? "";
    const payload = JSON.parse(Buffer.from(authorization.replace("Bearer ", "").split(".")[1] ?? "", "base64url").toString());
    expect(payload.iss).toBe(APP_ID);
    expect(payload.exp - payload.iat).toBe(600);
  });
});

describe("/v1/dispatch", () => {
  const dispatchRoutes = (file: string | null, extra: StubRoutes = {}): StubRoutes => ({
    [`GET ${repo}`]: () => json({ id: REPOSITORY, full_name: "lab/app", default_branch: "main" }),
    [`GET ${repo}/git/ref/heads/main`]: () => json({ ref: "refs/heads/main", object: { type: "commit", sha: HEAD } }),
    [`GET ${WORKFLOW_FILE}`]: (call) => file !== null && call.query.get("ref") === HEAD ? raw(file) : notFound(),
    [`POST ${DISPATCH}`]: () => json({ workflow_run_id: 7, run_url: `${API}${repo}/actions/runs/7`, html_url: "https://github.test/lab/app/actions/runs/7" }),
    ...extra,
  });
  const valid = { installation_id: INSTALLATION, repository_id: REPOSITORY, branch: "main", inputs: { build: "dep1.web", runner: "ubuntu-latest", commit: HEAD } };

  it("dispatches a canonical file at the branch head, with cloud from config", async () => {
    const minter = harness(dispatchRoutes(CANONICAL_WORKFLOW));
    const response = await minter.call("/v1/dispatch", valid);
    expect(response).toEqual({
      status: 200,
      body: { run_id: 7, run_url: "https://github.test/lab/app/actions/runs/7", head_sha: HEAD, workflow_ref: "lab/app/.github/workflows/ployz-build.yml@refs/heads/main" },
    });
    expect(sent(minter.calls, "POST", DISPATCH)[0]?.body).toEqual({
      ref: "refs/heads/main",
      inputs: { build: "dep1.web", runner: "ubuntu-latest", commit: HEAD, cloud: CLOUD },
      return_run_details: true,
    });
    expect(tokenRequests(minter.calls).map((call) => call.body)).toEqual([
      { permissions: { contents: "read", actions: "write" }, repository_ids: [REPOSITORY] },
    ]);
    expect(minter.audit[0]).toMatchObject({ outcome: "granted", ref: "main", head_sha: HEAD, run_id: 7, workflow_digest: expect.stringMatching(/^[0-9a-f]{64}$/) });
  });

  it("accepts a canonical file with CRLF line ends", async () => {
    const minter = harness(dispatchRoutes(CANONICAL_WORKFLOW.replaceAll("\n", "\r\n")));
    expect((await minter.call("/v1/dispatch", valid)).status).toBe(200);
  });

  const refusedFiles: [string, string | null][] = [
    ["a file one byte off every canonical template", CANONICAL_WORKFLOW.replace("Ployz build", "Ployz builds")],
    ["a file that asks for contents: write", CANONICAL_WORKFLOW.replace("contents: read", "contents: write")],
    ["a file that uses getployz/build@v1", TODAY_WORKFLOW],
    ["a missing file", null],
  ];

  it.each(refusedFiles)("refuses %s before GitHub runs anything", async (_, file) => {
    const minter = harness(dispatchRoutes(file));
    const response = await minter.call("/v1/dispatch", valid);
    expect(response.status).toBe(400);
    expect(sent(minter.calls, "POST", DISPATCH)).toEqual([]);
    expect(minter.audit[0]).toMatchObject({ outcome: "refused", head_sha: HEAD });
    if (file !== null) expect(minter.audit[0]?.workflow_digest).toMatch(/^[0-9a-f]{64}$/);
  });

  const refusedBodies: [string, unknown][] = [
    ["another workflow file name", { ...valid, workflow: "release.yml" }],
    ["a tag ref", { ...valid, branch: "refs/tags/v1" }],
    ["a full branch ref", { ...valid, branch: "refs/heads/main" }],
    ["a 40-hex ref", { ...valid, branch: HEAD }],
    ["a branch name git refuses", { ...valid, branch: "main..other" }],
    ["an input outside the allowed set", { ...valid, inputs: { ...valid.inputs, image: "evil" } }],
    ["a caller-set cloud", { ...valid, inputs: { ...valid.inputs, cloud: "https://evil.test" } }],
    ["a commit that is not 40 hex", { ...valid, inputs: { ...valid.inputs, commit: "abc123" } }],
    ["no build input", { ...valid, inputs: { runner: "ubuntu-latest" } }],
  ];

  it.each(refusedBodies)("refuses %s", async (_, body) => {
    const minter = harness(dispatchRoutes(CANONICAL_WORKFLOW));
    const response = await minter.call("/v1/dispatch", body);
    expect(response.status).toBe(400);
    expect(sent(minter.calls, "POST", DISPATCH)).toEqual([]);
  });

  it("refuses a branch GitHub doesn't have", async () => {
    const minter = harness(dispatchRoutes(CANONICAL_WORKFLOW));
    const response = await minter.call("/v1/dispatch", { ...valid, branch: "gone" });
    expect(response).toMatchObject({ status: 400, body: { reason: "branch not found" } });
  });

  it("refuses everything with the shipped placeholder set until M4 fills it", async () => {
    const { CANONICAL_DIGESTS } = await import("./policy.js");
    const minter = harness(dispatchRoutes(CANONICAL_WORKFLOW), { canonical: CANONICAL_DIGESTS });
    expect((await minter.call("/v1/dispatch", valid)).status).toBe(400);
  });
});

const RUN = {
  id: 7,
  name: "Ployz build",
  path: ".github/workflows/ployz-build.yml",
  html_url: "https://github.test/lab/app/actions/runs/7",
  head_sha: HEAD,
  event: "push",
  status: "completed",
  conclusion: "success",
  logs_url: `${API}${repo}/actions/runs/7/logs`,
  jobs_url: `${API}${repo}/actions/runs/7/jobs`,
  artifacts_url: `${API}${repo}/actions/runs/7/artifacts`,
  check_suite_url: `${API}${repo}/check-suites/1`,
  workflow_url: `${API}${repo}/actions/workflows/1`,
  actor: { login: "someone" },
};
const OTHER_RUN = { ...RUN, id: 8, path: ".github/workflows/release.yml", html_url: "https://github.test/lab/app/actions/runs/8" };
const runRoutes: StubRoutes = {
  [`GET ${repo}/actions/runs/7`]: () => json(RUN),
  [`GET ${repo}/actions/runs/8`]: () => json(OTHER_RUN),
  [`POST ${repo}/actions/runs/7/cancel`]: () => json({}, 202),
  [`POST ${repo}/actions/runs/8/cancel`]: () => json({}, 202),
  [`GET ${repo}/actions/workflows/ployz-build.yml/runs`]: () => json({ total_count: 2, workflow_runs: [RUN, OTHER_RUN] }),
};

describe("run routes", () => {
  it("/v1/run returns only the run's metadata", async () => {
    const minter = harness(runRoutes);
    const response = await minter.call("/v1/run", { installation_id: INSTALLATION, repository_id: REPOSITORY, run_id: 7 });
    expect(response.body).toStrictEqual({ id: 7, url: RUN.html_url, head_sha: HEAD, event: "push", status: "completed", conclusion: "success" });
    expect(tokenRequests(minter.calls)[0]?.body).toEqual({ permissions: { actions: "read" }, repository_ids: [REPOSITORY] });
  });

  it("/v1/runs lists only build workflow runs, with only their metadata", async () => {
    const minter = harness(runRoutes);
    const response = await minter.call("/v1/runs", { installation_id: INSTALLATION, repository_id: REPOSITORY, head_sha: HEAD, event: "push" });
    expect(response.body).toStrictEqual({ runs: [{ id: 7, url: RUN.html_url, head_sha: HEAD, event: "push", status: "completed" }] });
    const [list] = sent(minter.calls, "GET", `${repo}/actions/workflows/ployz-build.yml/runs`);
    expect(Object.fromEntries(list?.query ?? [])).toEqual({ head_sha: HEAD, event: "push", per_page: "20" });
  });

  it.each(["/v1/run", "/v1/cancel"])("%s refuses a run of another workflow", async (path) => {
    const minter = harness(runRoutes);
    const response = await minter.call(path, { installation_id: INSTALLATION, repository_id: REPOSITORY, run_id: 8 });
    expect(response).toMatchObject({ status: 400, body: { reason: "the run is not of the build workflow" } });
    expect(sent(minter.calls, "POST", `${repo}/actions/runs/8/cancel`)).toEqual([]);
  });

  it("/v1/runs refuses a workflow field", async () => {
    const minter = harness(runRoutes);
    const response = await minter.call("/v1/runs", { installation_id: INSTALLATION, repository_id: REPOSITORY, head_sha: HEAD, event: "push", workflow: "release.yml" });
    expect(response.status).toBe(400);
  });

  it("/v1/cancel cancels a build run, and takes a finished one as done", async () => {
    const minter = harness({ ...runRoutes, [`POST ${repo}/actions/runs/7/cancel`]: () => json({ message: "Cannot cancel a workflow run that is completed." }, 409) });
    const response = await minter.call("/v1/cancel", { installation_id: INSTALLATION, repository_id: REPOSITORY, run_id: 7 });
    expect(response).toEqual({ status: 200, body: {} });
    expect(sent(minter.calls, "POST", `${repo}/actions/runs/7/cancel`)).toHaveLength(1);
    expect(tokenRequests(minter.calls)[0]?.body).toEqual({ permissions: { actions: "write" }, repository_ids: [REPOSITORY] });
    expect(minter.audit[0]).toMatchObject({ outcome: "granted", run_id: 7, github_status: 409 });
  });
});

describe("/v1/check", () => {
  const CHECK = "Ployz · ready to merge";
  const checkRuns = `${repo}/commits/${HEAD}/check-runs`;
  const check = {
    installation_id: INSTALLATION, repository_id: REPOSITORY, head_sha: HEAD, name: CHECK, external_id: "pr-3", status: "completed", conclusion: "success",
    details_url: "https://cloud.ployz.test/cloud/acme", output: { title: "Lands web", summary: "web 1.2" },
  };

  it("refuses a name outside the Ployz set", async () => {
    const minter = harness();
    const response = await minter.call("/v1/check", { ...check, name: "CI / tests" });
    expect(response.status).toBe(400);
    expect(minter.calls).toEqual([]);
  });

  it("refuses a conclusion without a completed status", async () => {
    const minter = harness();
    expect((await minter.call("/v1/check", { ...check, status: "in_progress" })).status).toBe(400);
  });

  it("creates the check when the commit has none of the App's", async () => {
    const minter = harness({
      [`GET ${checkRuns}`]: () => json({ check_runs: [{ id: 3, external_id: "pr-3", app: { id: 999 } }] }),
      [`POST ${repo}/check-runs`]: () => json({ id: 5 }, 201),
    });
    const response = await minter.call("/v1/check", check);
    expect(response.body).toEqual({ check_run_id: 5 });
    const [created] = sent(minter.calls, "POST", `${repo}/check-runs`);
    expect(created?.body).toEqual({
      name: CHECK, status: "completed", conclusion: "success", external_id: "pr-3", details_url: check.details_url, output: check.output, head_sha: HEAD,
    });
    expect(tokenRequests(minter.calls)[0]?.body).toEqual({ permissions: { checks: "write" }, repository_ids: [REPOSITORY] });
  });

  it("updates the App's existing check in place", async () => {
    const minter = harness({
      [`GET ${checkRuns}`]: () => json({ check_runs: [{ id: 4, external_id: "pr-3", app: { id: Number(APP_ID) } }] }),
      [`PATCH ${repo}/check-runs/4`]: () => json({ id: 4 }),
    });
    expect((await minter.call("/v1/check", check)).body).toEqual({ check_run_id: 4 });
    expect(sent(minter.calls, "POST", `${repo}/check-runs`)).toEqual([]);
  });

  it("updates in place a check posted without an external_id, which GitHub lists with an empty one", async () => {
    const minter = harness({
      [`GET ${checkRuns}`]: () => json({ check_runs: [{ id: 6, external_id: "", app: { id: Number(APP_ID) } }] }),
      [`PATCH ${repo}/check-runs/6`]: () => json({ id: 6 }),
    });
    const { external_id: _, ...unkeyed } = check;
    expect((await minter.call("/v1/check", unkeyed)).body).toEqual({ check_run_id: 6 });
    expect(sent(minter.calls, "POST", `${repo}/check-runs`)).toEqual([]);
  });
});

describe("/v1/connect", () => {
  const connectRoutes: StubRoutes = {
    "POST /login/oauth/access_token": (call) => {
      const body = call.body as Record<string, string>;
      return body["client_secret"] === CLIENT_SECRET && body["code"] === CODE ? json({ access_token: USER_TOKEN, token_type: "bearer" }) : json({ error: "bad_verification_code" });
    },
    "GET /user": (call) => call.headers.get("authorization") === `Bearer ${USER_TOKEN}` ? json({ login: "Octo" }) : json({}, 401),
    "GET /user/installations": () => json({
      installations: [
        { id: 1, account: { login: "acme", type: "Organization", avatar_url: null }, suspended_at: null },
        { id: 2, account: { login: "octo", type: "User", avatar_url: "https://avatars.test/2" }, suspended_at: null },
        { id: 3, account: { login: "other", type: "Organization", avatar_url: null }, suspended_at: "2026-01-01T00:00:00Z" },
        { id: 4, account: { login: "sso", type: "Organization", avatar_url: null }, suspended_at: null },
      ],
    }),
    "GET /user/memberships/orgs/acme": () => json({ state: "active", role: "admin" }),
    "GET /user/memberships/orgs/sso": () => json({ message: "Resource protected by organization SAML enforcement." }, 403),
  };

  it("returns the login and installations, unknown where GitHub won't say who administers one, and never the user token", async () => {
    const minter = harness(connectRoutes);
    const response = await minter.call("/v1/connect", { code: CODE });
    expect(response.body).toEqual({
      login: "Octo",
      installations: [
        { id: 1, account: { login: "acme", type: "Organization", avatar_url: null }, suspended_at: null, administers: true },
        { id: 2, account: { login: "octo", type: "User", avatar_url: "https://avatars.test/2" }, suspended_at: null, administers: true },
        { id: 3, account: { login: "other", type: "Organization", avatar_url: null }, suspended_at: "2026-01-01T00:00:00Z", administers: false },
        { id: 4, account: { login: "sso", type: "Organization", avatar_url: null }, suspended_at: null, administers: null },
      ],
    });
    expect(JSON.stringify([response, minter.audit])).not.toContain(USER_TOKEN);
    expect(JSON.stringify([response, minter.audit])).not.toContain(CODE);
  });

  it("fails without echoing the code when GitHub refuses it", async () => {
    const minter = harness(connectRoutes);
    const response = await minter.call("/v1/connect", { code: "wrong-code" });
    expect(response).toMatchObject({ status: 502, body: { error: "github", operation: "exchange_user_code" } });
  });
});

describe("/v1/workflow and /v1/installation", () => {
  it("reports a canonical file on the default branch", async () => {
    const minter = harness({
      [`GET ${repo}`]: () => json({ full_name: "lab/app", default_branch: "trunk" }),
      [`GET ${repo}/actions/workflows/ployz-build.yml`]: () => json({ path: ".github/workflows/ployz-build.yml", state: "active" }),
      [`GET ${WORKFLOW_FILE}`]: (call) => call.query.get("ref") === "trunk" ? raw(CANONICAL_WORKFLOW) : notFound(),
    });
    const response = await minter.call("/v1/workflow", { installation_id: INSTALLATION, repository_id: REPOSITORY });
    expect(response.body).toMatchObject({ full_name: "lab/app", default_branch: "trunk", state: "active", canonical: true });
  });

  it("reports a repository with no workflow", async () => {
    const minter = harness({ [`GET ${repo}`]: () => json({ full_name: "lab/app", default_branch: "main" }) });
    const response = await minter.call("/v1/workflow", { installation_id: INSTALLATION, repository_id: REPOSITORY });
    expect(response.body).toEqual({ full_name: "lab/app", default_branch: "main", state: null, digest: null, canonical: false });
  });

  it("returns the installation's URL and permissions", async () => {
    const minter = harness({
      [`GET /app/installations/${INSTALLATION}`]: () => json({ id: INSTALLATION, html_url: "https://github.test/settings/installations/11", permissions: { checks: "write" }, access_tokens_url: "x" }),
    });
    const response = await minter.call("/v1/installation", { installation_id: INSTALLATION });
    expect(response.body).toStrictEqual({ url: "https://github.test/settings/installations/11", permissions: { checks: "write" } });
  });
});

describe("the request", () => {
  it("refuses an unknown route and audits it", async () => {
    const minter = harness();
    const response = await minter.call("/v1/logs", { installation_id: INSTALLATION });
    expect(response.status).toBe(404);
    expect(minter.audit[0]).toMatchObject({ route: "/v1/logs", outcome: "refused", reason: "unknown route" });
  });

  it("refuses a caller the transport didn't authenticate", async () => {
    const minter = harness();
    const response = await minter.handle({ method: "POST", path: "/v1/token", caller: null, body: { installation_id: INSTALLATION } });
    expect(response.status).toBe(403);
    expect(minter.calls).toEqual([]);
    expect(minter.audit[0]).toMatchObject({ caller: "unauthenticated", outcome: "refused" });
  });

  it("refuses a method other than POST", async () => {
    const minter = harness();
    expect((await minter.handle({ method: "GET", path: "/v1/token", caller: "test", body: undefined })).status).toBe(405);
  });

  it("passes GitHub's status and retry hint through, without its body", async () => {
    const minter = harness({
      [`POST /app/installations/${INSTALLATION}/access_tokens`]: () => new Response(JSON.stringify({ message: "rate limited" }), { status: 429, headers: { "retry-after": "30" } }),
    });
    const response = await minter.call("/v1/token", { installation_id: INSTALLATION });
    expect(response).toEqual({ status: 502, body: { error: "github", operation: "installation_token", failure: "status", status: 429, retry_after: 30 } });
    expect(minter.audit[0]).toMatchObject({ outcome: "failed", github_status: 429 });
  });

  it("tells a GitHub that never answered apart from one that answered a shape it can't read", async () => {
    const mint = `POST /app/installations/${INSTALLATION}/access_tokens`;
    const down = harness({ [mint]: () => { throw new TypeError("fetch failed"); } });
    expect(await down.call("/v1/token", { installation_id: INSTALLATION }))
      .toEqual({ status: 502, body: { error: "github", operation: "installation_token", failure: "unreachable" } });
    const garbled = harness({ [mint]: () => json({ token: 7 }, 201) });
    expect(await garbled.call("/v1/token", { installation_id: INSTALLATION }))
      .toEqual({ status: 502, body: { error: "github", operation: "installation_token", failure: "unreadable" } });
  });

  it("reads a body cut off after its headers as unreachable, and a whole body that doesn't parse as unreadable", async () => {
    const mint = `POST /app/installations/${INSTALLATION}/access_tokens`;
    const cutOff = () => new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode("{\"tok"));
        controller.error(new TypeError("terminated"));
      },
    }), { status: 201 });
    expect(await harness({ [mint]: cutOff }).call("/v1/token", { installation_id: INSTALLATION }))
      .toEqual({ status: 502, body: { error: "github", operation: "installation_token", failure: "unreachable" } });
    expect(await harness({ [mint]: () => new Response("{\"tok", { status: 201 }) }).call("/v1/token", { installation_id: INSTALLATION }))
      .toEqual({ status: 502, body: { error: "github", operation: "installation_token", failure: "unreadable" } });
  });
});
