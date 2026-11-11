import { ConfigProvider, Effect, Layer } from "effect";
import { TestClock } from "effect/testing";
import { afterEach, describe, expect, it, vi } from "vitest";
import { GITHUB_BUILD_WORKFLOW } from "./github-build-workflow";
import { GITHUB_READ_SET, GithubAuthority, GithubAuthorityLive, type GithubAuthorityService } from "./github-authority.server";
import { testConfigEnvironment, testConfigLayer } from "#/test/config-environment";
import { AppConfig } from "#/server/config.server";

type Sent = { url: URL; method: string; headers: Headers; body: unknown };

/** GitHub, or the minter, answering each request with what `answer` gives for it; `sent` keeps every request. */
function serve(answer: (request: Sent) => Response | undefined) {
  const sent: Sent[] = [];
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init);
    const text = await request.text();
    const recorded = { url: new URL(request.url), method: request.method, headers: request.headers, body: text === "" ? undefined : JSON.parse(text) };
    sent.push(recorded);
    return answer(recorded) ?? Response.json({ message: "Not Found" }, { status: 404 });
  });
  return sent;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

// AppConfig requires a database URL; nothing here connects to it.
const DATABASE_URL = "postgres://postgres:postgres@127.0.0.1:9/ployz_cloud";
const keyless = () => Object.fromEntries(Object.entries({ ...testConfigEnvironment(), DATABASE_URL })
  .filter(([name]) => name !== "GITHUB_APP_PRIVATE_KEY" && name !== "GITHUB_APP_CLIENT_SECRET"));
const minterUrl = "https://abc123.lambda-url.eu-central-1.on.aws";
const minterConfig = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
  env: { ...keyless(), GITHUB_MINTER_URL: minterUrl, GITHUB_MINTER_ACCESS_KEY_ID: "AKIAEXAMPLE", GITHUB_MINTER_SECRET_ACCESS_KEY: "minter-secret" },
}))));

const withAuthority = <A>(config: Layer.Layer<AppConfig, unknown>, use: (authority: GithubAuthorityService) => Effect.Effect<A, unknown>) =>
  Effect.runPromise(Effect.gen(function* () { return yield* use(yield* GithubAuthority); }).pipe(Effect.provide(GithubAuthorityLive.pipe(Layer.provide(config)))));
const local = <A>(use: (authority: GithubAuthorityService) => Effect.Effect<A, unknown>) => withAuthority(testConfigLayer({ DATABASE_URL }), use);
const viaMinter = <A>(use: (authority: GithubAuthorityService) => Effect.Effect<A, unknown>) => withAuthority(minterConfig, use);

const token = (permissions: Record<string, string>) => Response.json({ token: "minted", expires_at: "2099-01-01T00:00:00Z", permissions });
const minted = (sent: Sent[]) => sent.filter((request) => request.url.pathname.endsWith("/access_tokens")).map((request) => request.body);
const HEAD = "c".repeat(40);
const file = (text: string) => Response.json({ type: "file", encoding: "base64", content: Buffer.from(text).toString("base64") });

/** `acme/web` (42) with `ployz-build.yml` on main's head holding `workflow`, and the build workflow's run 9001. */
function acmeWeb(workflow: string, runPath = ".github/workflows/ployz-build.yml") {
  return serve(({ url, method }) => {
    if (url.pathname === "/app/installations/7/access_tokens") return token({ contents: "read" });
    if (url.pathname === "/repositories/42") return Response.json({ full_name: "acme/web", default_branch: "main" });
    if (url.pathname === "/repositories/42/git/ref/heads/main") return Response.json({ object: { type: "commit", sha: HEAD } });
    if (url.pathname === "/repositories/42/contents/.github/workflows/ployz-build.yml" && url.searchParams.get("ref") === HEAD) return file(workflow);
    if (url.pathname === "/repositories/42/actions/workflows/ployz-build.yml/dispatches" && method === "POST") {
      return Response.json({ workflow_run_id: 9001, html_url: "https://github.com/acme/web/actions/runs/9001" });
    }
    if (url.pathname === "/repositories/42/actions/runs/9001") {
      return Response.json({ id: 9001, html_url: "https://github.com/acme/web/actions/runs/9001", head_sha: HEAD, event: "push", path: runPath, status: "completed", conclusion: "success" });
    }
    if (url.pathname === "/repositories/42/actions/runs/9001/cancel") return new Response(null, { status: 202 });
    return undefined;
  });
}

describe("the App's authority held in Cloud", () => {
  it("asks GitHub for read tokens no wider than the read set, and narrower when told", async () => {
    const sent = serve(({ url }) => url.pathname === "/app/installations/7/access_tokens" ? token({ contents: "read", metadata: "read" }) : undefined);
    await local((authority) => Effect.all([authority.readToken(7), authority.readToken(7, { repositoryIds: [42], permissions: { contents: "read" } })]));
    expect(minted(sent)).toEqual([{ permissions: GITHUB_READ_SET }, { permissions: { contents: "read" }, repository_ids: [42] }]);
    expect(sent[0]?.headers.get("authorization")).toMatch(/^Bearer [\w-]+\.[\w-]+\.[\w-]+$/);
  });

  it("asks only for the read permissions an installation accepted, when GitHub refuses the whole read set", async () => {
    const sent = serve(({ url, body }) => {
      if (url.pathname === "/app/installations/7") return Response.json({ html_url: "https://github.com/settings/installations/7", permissions: { contents: "read", metadata: "read", actions: "write" } });
      if (url.pathname !== "/app/installations/7/access_tokens") return undefined;
      return JSON.stringify(body).includes("checks") ? Response.json({ message: "The permissions requested are not granted to this installation." }, { status: 422 }) : token({ contents: "read", metadata: "read" });
    });
    expect(await local((authority) => authority.readToken(7))).toBe("minted");
    expect(minted(sent)).toEqual([{ permissions: GITHUB_READ_SET }, { permissions: { contents: "read", metadata: "read" } }]);
  });

  it("keeps an operation's token for its repository, and mints again for another need", async () => {
    const sent = acmeWeb(GITHUB_BUILD_WORKFLOW);
    await local((authority) => Effect.all([
      authority.buildRun({ installationId: 7, repositoryId: 42, runId: 9001 }),
      authority.buildRun({ installationId: 7, repositoryId: 42, runId: 9001 }),
      authority.cancelBuild({ installationId: 7, repositoryId: 42, runId: 9001 }),
    ]));
    expect(minted(sent)).toEqual([{ permissions: { actions: "read" }, repository_ids: [42] }, { permissions: { actions: "write" }, repository_ids: [42] }]);
  });

  it("mints an operation's token per repository, and again once the kept one nears expiry", async () => {
    const sent = serve(({ url }) => {
      if (url.pathname.endsWith("/access_tokens")) return token({ actions: "read" });
      const run = /^\/repositories\/(\d+)\/actions\/runs\/9001$/.exec(url.pathname);
      return run === null ? undefined : Response.json({
        id: 9001, html_url: "https://github.com/acme/web/actions/runs/9001", head_sha: HEAD, event: "push", path: ".github/workflows/ployz-build.yml", status: "completed", conclusion: "success",
      });
    });
    await local((authority) => Effect.gen(function* () {
      yield* authority.buildRun({ installationId: 7, repositoryId: 42, runId: 9001 });
      yield* authority.buildRun({ installationId: 7, repositoryId: 43, runId: 9001 });
      yield* TestClock.adjust("49 minutes");
      yield* authority.buildRun({ installationId: 7, repositoryId: 42, runId: 9001 });
      yield* TestClock.adjust("2 minutes");
      yield* authority.buildRun({ installationId: 7, repositoryId: 42, runId: 9001 });
    }).pipe(Effect.provide(TestClock.layer())));
    expect(minted(sent)).toEqual([
      { permissions: { actions: "read" }, repository_ids: [42] },
      { permissions: { actions: "read" }, repository_ids: [43] },
      { permissions: { actions: "read" }, repository_ids: [42] },
    ]);
  });

  it("refuses a read token GitHub granted more than reading", async () => {
    serve(({ url }) => url.pathname === "/app/installations/7/access_tokens" ? token({ contents: "read", actions: "write" }) : undefined);
    await expect(local((authority) => authority.readToken(7))).rejects.toMatchObject({ code: "invalid_response", operation: "installation_token" });
  });

  it("dispatches the build workflow with a token for that one repository, and names its own Cloud", async () => {
    const sent = acmeWeb(GITHUB_BUILD_WORKFLOW);
    const run = await local((authority) => authority.dispatchBuild({ installationId: 7, repositoryId: 42, branch: "main", inputs: { build: "b", runner: "ubuntu-latest" } }));
    expect(run).toEqual({ runId: 9001, runUrl: "https://github.com/acme/web/actions/runs/9001", headSha: HEAD, workflowRef: "acme/web/.github/workflows/ployz-build.yml@refs/heads/main" });
    expect(minted(sent)).toEqual([{ permissions: { contents: "read", actions: "write" }, repository_ids: [42] }]);
    expect(sent.at(-1)?.body).toEqual({ ref: "refs/heads/main", inputs: { build: "b", runner: "ubuntu-latest", cloud: "http://localhost:3000" }, return_run_details: true });
  });

  it("dispatches a repository's own edit of the build workflow, and reads no file to decide", async () => {
    const sent = acmeWeb(`${GITHUB_BUILD_WORKFLOW}\n      - run: echo customized\n`);
    await local((authority) => authority.dispatchBuild({ installationId: 7, repositoryId: 42, branch: "main", inputs: { build: "b" } }));
    expect(sent.some((request) => request.method === "POST" && request.url.pathname.endsWith("/dispatches"))).toBe(true);
    expect(sent.some((request) => request.url.pathname.includes("/contents/"))).toBe(false);
    const workflow = await local((authority) => authority.buildWorkflow({ installationId: 7, repositoryId: 42 }));
    expect(workflow.dispatchable).toBe(true);
  });

  it("dispatches a commit a person approved on the rule's branch, with a token that can only run Actions there", async () => {
    // The rule's own template on its branch, which names its Cloud and builder: no digest of Ployz's applies.
    const sent = acmeWeb("name: Ployz build\non: { push: { branches: [main] } }\n");
    const run = await local((authority) => authority.dispatchApprovedBuild({ installationId: 7, repositoryId: 42, branch: "main", commit: HEAD }));
    expect(run).toEqual({ runId: 9001, runUrl: "https://github.com/acme/web/actions/runs/9001" });
    expect(minted(sent)).toEqual([{ permissions: { actions: "write" }, repository_ids: [42] }]);
    expect(sent.at(-1)?.body).toEqual({ ref: "refs/heads/main", inputs: { commit: HEAD }, return_run_details: true });
  });

  it("cancels only a run of the build workflow", async () => {
    const sent = acmeWeb(GITHUB_BUILD_WORKFLOW, ".github/workflows/release.yml");
    await expect(local((authority) => authority.cancelBuild({ installationId: 7, repositoryId: 42, runId: 9001 })))
      .rejects.toMatchObject({ code: "refused", operation: "cancel_run" });
    expect(sent.some((request) => request.url.pathname.endsWith("/cancel"))).toBe(false);
    expect(minted(sent)).toEqual([{ permissions: { actions: "write" }, repository_ids: [42] }]);
  });

  it("reads every page of a person's installations and whether they own each account", async () => {
    const reached = Array.from({ length: 130 }, (_, index) => ({
      id: index + 1, account: { login: index === 0 ? "ada" : `org-${index}`, type: index === 0 ? "User" : "Organization", avatar_url: null }, suspended_at: null,
    }));
    const sent = serve(({ url, method }) => {
      if (url.pathname === "/login/oauth/access_token" && method === "POST") return Response.json({ access_token: "user-token" });
      if (url.pathname === "/user") return Response.json({ login: "ada" });
      if (url.pathname === "/user/installations") {
        const page = Number(url.searchParams.get("page"));
        return Response.json({ installations: reached.slice((page - 1) * 100, page * 100) });
      }
      if (url.pathname === "/user/memberships/orgs/org-1") return Response.json({ state: "active", role: "admin" });
      if (url.pathname === "/user/memberships/orgs/org-2") return Response.json({ message: "Resource protected by organization SAML enforcement." }, { status: 403 });
      if (url.pathname.startsWith("/user/memberships/orgs/")) return Response.json({ state: "active", role: "member" });
      return undefined;
    });
    const user = await local((authority) => authority.connect("code-1"));
    expect(user.login).toBe("ada");
    expect(user.installations).toHaveLength(130);
    expect(user.installations.filter((installation) => installation.administers).map((installation) => installation.id)).toEqual([1, 2]);
    expect(user.installations.filter((installation) => installation.administers === null).map((installation) => installation.id)).toEqual([3]);
    expect(sent.filter((request) => request.url.pathname === "/user/installations").map((request) => request.url.searchParams.get("page"))).toEqual(["1", "2"]);
    expect(sent[0]?.url.origin).toBe("https://github.com");
    expect(sent[0]?.headers.get("accept")).toBe("application/json");
    expect(sent[0]?.body).toMatchObject({ code: "code-1" });
  });

  it("tries a mint again when GitHub cut its body short or answered a bare 403", async () => {
    serve(() => new Response("{\"tok", { status: 201 }));
    await expect(local((authority) => authority.readToken(7))).rejects.toMatchObject({ code: "request_failed", retriable: true });
    serve(() => new Response("", { status: 403 }));
    await expect(local((authority) => authority.readToken(7))).rejects.toMatchObject({ code: "request_failed", status: 403, retriable: true });
  });
});

/** What the minter answers when a route fails: its refusal, GitHub's failure, or its own. */
type MinterAnswer =
  | { readonly error: "refused"; readonly reason: string }
  | { readonly error: "github"; readonly operation: string; readonly failure: "status"; readonly status: number; readonly retry_after?: number }
  | { readonly error: "github"; readonly operation: string; readonly failure: "unreachable" | "unreadable" | "overbroad" }
  | { readonly error: "internal" }
  | { readonly message: string };

describe("the App's authority held by the minter", () => {
  it("signs each request for the minter's Lambda Function URL, and sends the token's scope as given", async () => {
    const sent = serve(() => Response.json({ token: "minted", expires_at: "2099-01-01T00:00:00Z" }));
    expect(await viaMinter((authority) => authority.readToken(7, { repositoryIds: [42], permissions: { contents: "read" } }))).toBe("minted");
    const [request] = sent;
    expect(request?.url.href).toBe(`${minterUrl}/v1/token`);
    expect(request?.method).toBe("POST");
    expect(request?.headers.get("authorization")).toMatch(/^AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE\/\d{8}\/eu-central-1\/lambda\/aws4_request, SignedHeaders=[^,]+, Signature=[0-9a-f]{64}$/);
    expect(request?.headers.get("x-amz-date")).toMatch(/^\d{8}T\d{6}Z$/);
    expect(request?.body).toEqual({ installation_id: 7, repository_ids: [42], permissions: { contents: "read" } });
  });

  it("asks only for the read permissions an installation accepted, when the minter reports GitHub refused the whole set", async () => {
    const sent = serve(({ url, body }) => {
      if (url.pathname === "/v1/installation") return Response.json({ url: "https://github.com/settings/installations/7", permissions: { contents: "read", metadata: "read" } });
      return JSON.stringify(body).includes("checks")
        ? Response.json({ error: "github", operation: "installation_token", failure: "status", status: 422 }, { status: 502 })
        : Response.json({ token: "minted", expires_at: "2099-01-01T00:00:00Z" });
    });
    expect(await viaMinter((authority) => authority.readToken(7))).toBe("minted");
    expect(sent.map((request) => [request.url.pathname, request.body])).toEqual([
      ["/v1/token", { installation_id: 7, permissions: GITHUB_READ_SET }],
      ["/v1/installation", { installation_id: 7 }],
      ["/v1/token", { installation_id: 7, permissions: { contents: "read", metadata: "read" } }],
    ]);
  });

  it("reads a build workflow as dispatchable only when the minter found it canonical", async () => {
    const workflow = (canonical: boolean) => {
      serve(() => Response.json({ full_name: "acme/web", default_branch: "main", state: "active", digest: "d", canonical }));
      return viaMinter((authority) => authority.buildWorkflow({ installationId: 7, repositoryId: 42 }));
    };
    expect(await workflow(true)).toEqual({ fullName: "acme/web", defaultBranch: "main", state: "active", dispatchable: true });
    expect((await workflow(false)).dispatchable).toBe(false);
  });

  it("leaves the Cloud input of a dispatch to the minter", async () => {
    const sent = serve(() => Response.json({ run_id: 9001, run_url: "https://github.com/acme/web/actions/runs/9001", head_sha: HEAD, workflow_ref: "w" }));
    await viaMinter((authority) => authority.dispatchBuild({ installationId: 7, repositoryId: 42, branch: "main", inputs: { build: "b", runner: "ubuntu-latest" } }));
    expect(sent[0]?.body).toEqual({ installation_id: 7, repository_id: 42, branch: "main", inputs: { build: "b", runner: "ubuntu-latest" } });
  });

  it("reads the minter's answers as the errors GitHub itself would have given", async () => {
    const cancel = (status: number, body: MinterAnswer) => {
      serve(() => Response.json(body, { status }));
      return viaMinter((authority) => authority.cancelBuild({ installationId: 7, repositoryId: 42, runId: 9001 }));
    };
    await expect(cancel(400, { error: "refused", reason: "not a build workflow run" }))
      .rejects.toMatchObject({ code: "refused", operation: "cancel_run", retriable: false, message: expect.stringContaining("not a build workflow run") });
    await expect(cancel(502, { error: "github", operation: "cancel_run", failure: "status", status: 404 })).rejects.toMatchObject({ code: "not_found", status: 404, retriable: false });
    await expect(cancel(502, { error: "github", operation: "cancel_run", failure: "status", status: 503 })).rejects.toMatchObject({ code: "request_failed", retriable: true });
    await expect(cancel(502, { error: "github", operation: "cancel_run", failure: "status", status: 403, retry_after: 60 }))
      .rejects.toMatchObject({ retriable: true, retryAfterSeconds: 60 });
    await expect(cancel(502, { error: "github", operation: "cancel_run", failure: "status", status: 403 })).rejects.toMatchObject({ retriable: false });
    await expect(cancel(502, { error: "github", operation: "installation_token", failure: "status", status: 403 }))
      .rejects.toMatchObject({ operation: "installation_token", retriable: true });
    await expect(cancel(502, { error: "github", operation: "installation_token", failure: "unreachable" }))
      .rejects.toMatchObject({ code: "request_failed", operation: "installation_token", retriable: true });
    await expect(cancel(502, { error: "github", operation: "cancel_run", failure: "unreachable" }))
      .rejects.toMatchObject({ code: "request_failed", operation: "cancel_run", retriable: true });
    await expect(cancel(502, { error: "github", operation: "cancel_run", failure: "unreadable" }))
      .rejects.toMatchObject({ code: "invalid_response", operation: "cancel_run", retriable: false });
    await expect(cancel(502, { error: "github", operation: "installation_token_granted_more", failure: "overbroad" }))
      .rejects.toMatchObject({ code: "invalid_response", retriable: false });
    await expect(cancel(500, { error: "internal" })).rejects.toMatchObject({ code: "request_failed", retriable: true });
    await expect(cancel(403, { message: "Forbidden" })).rejects.toMatchObject({ code: "request_failed", retriable: false });
  });

  it("runs no build again, and dispatches no rule's workflow", async () => {
    const sent = serve(() => undefined);
    await expect(viaMinter((authority) => authority.rerunBuild({ installationId: 7, repositoryId: 42, runId: 9001 })))
      .rejects.toMatchObject({ code: "refused", operation: "rerun_run" });
    await expect(viaMinter((authority) => authority.dispatchApprovedBuild({ installationId: 7, repositoryId: 42, branch: "main", commit: HEAD })))
      .rejects.toMatchObject({ code: "refused", operation: "dispatch_workflow" });
    expect(sent).toEqual([]);
  });
});
