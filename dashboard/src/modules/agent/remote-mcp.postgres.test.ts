import { createHash, randomBytes } from "node:crypto";
import { it } from "@effect/vitest";
import { Client, StreamableHTTPClientTransport } from "@modelcontextprotocol/client";
import { sql } from "drizzle-orm";
import { Effect, Layer, Schema } from "effect";
import { afterEach, expect, vi } from "vitest";
import { connectAgent } from "#/modules/agent/connect.server";
import { serveAuth, serveMcpMetadata, serveRemoteMcp } from "#/modules/agent/remote-mcp.server";
import { decideApproval, pendingApprovals } from "#/modules/approvals/approvals.server";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import type { Caller } from "#/modules/identity/actor";
import type { JsonObject } from "#/db/tables";
import { oauthConsent } from "#/modules/identity/oauth-tables";
import { member } from "#/modules/identity/tables";
import { REQUEST_KEY_HEADER } from "#/modules/operations/keyed.server";
import { Auth, AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const origin = "http://localhost:3000";
const REDIRECT = "http://127.0.0.1:9999/callback";
const ORGANIZATION = "00000000-0000-4000-8000-00000000a801";
const PROJECT = "00000000-0000-4000-8000-00000000a802";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a803";
const SERVICE = "00000000-0000-4000-8000-00000000a804";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000a805";
const SHOP = { project: "shop", environment: "production" };

const HOSTED_ENV = {
  POLAR_ACCESS_TOKEN: "polar-token",
  POLAR_WEBHOOK_SECRET: "polar-webhook-secret",
  POLAR_PRODUCT_ID: "22222222-2222-4222-8222-222222222222",
  POLAR_SERVER: "sandbox",
};

const hostedPolar: PolarService = {
  mode: "hosted",
  productId: HOSTED_ENV.POLAR_PRODUCT_ID,
  listActiveSubscriptions: () => Effect.die("not used"),
  createCustomerPortal: () => Effect.die("not used"),
};

const Registered = Schema.Struct({ client_id: Schema.String });
const Issued = Schema.Struct({ access_token: Schema.String, refresh_token: Schema.optional(Schema.String) });
const Consents = Schema.Array(Schema.Struct({ id: Schema.String }));
const Structured = Schema.Struct({
  ok: Schema.Boolean,
  request_key: Schema.String,
  approval_required: Schema.optional(Schema.Struct({ approval_id: Schema.String, url: Schema.String })),
  refusal: Schema.optional(Schema.Struct({ code: Schema.String })),
  value: Schema.optional(Schema.Unknown),
});

afterEach(() => {
  vi.unstubAllGlobals();
});

/** A Cloud, hosted or self-hosted, with every route the remote endpoint answers on, run as a browser or agent would. */
const cloudOf = Effect.fn(function* (hosted: boolean) {
  const cloud = yield* storeTestCloud(hosted ? { env: HOSTED_ENV, polar: hostedPolar } : {});
  const services = yield* Layer.build(Layer.merge(AuthLive.pipe(Layer.provide(cloud)), cloud));
  const run = Effect.runPromiseWith(services);
  const provided = <A, E>(effect: Effect.Effect<A, E, Layer.Success<typeof cloud> | Auth>) => effect.pipe(Effect.provide(services));
  return { run, provided };
});

/**
 * Hosted Cloud with Shop: `web` deployed and staged for removal, so the next Deploy destroys it. Bo signed up and joined
 * Shop as an owner; the app's own JWKS answers through its auth routes.
 */
const hostedShop = Effect.fn(function* () {
  const { run, provided } = yield* cloudOf(true);
  const adaId = yield* provided(seedStoreOrganization(ORGANIZATION));
  yield* provided(enrollStoreServer(ORGANIZATION));
  const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  const here = { project: null, environment: null };
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
  }, acmeWeb()));
  yield* provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update config_deployment set status = 'applied' where id = ${DEPLOYMENT}`);
    yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      select environment_id, service->>'id', organization_id, ${DEPLOYMENT}, 'service', service::text
      from config_saved, lateral jsonb_array_elements(intent::jsonb->'services') service where environment_id = ${ENVIRONMENT}`);
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "remove_service", environment: here, service: "web" }));

  const signUp = yield* provided(Effect.gen(function* () {
    const auth = yield* Auth;
    return yield* auth.handler(new Request(`${origin}/api/auth/sign-up/email`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email: "bo@example.test", name: "bo", password: "correct-horse-battery-staple" }),
    }));
  }));
  const cookie = signUp.headers.get("set-cookie")?.split(";", 1)[0] ?? expect.fail("no session cookie");
  const boId = yield* provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [bo] = yield* drizzle.execute<{ id: string }>(sql`select id from "user" where email = 'bo@example.test'`, "objects");
    const id = bo?.id ?? expect.fail("bo was not created");
    yield* drizzle.insert(member).values({ userId: id, organizationId: ORGANIZATION, role: "owner" });
    return id;
  }));

  const real = globalThis.fetch;
  vi.stubGlobal("fetch", (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init);
    return request.url.startsWith(`${origin}/api/auth/`) ? run(serveAuth(request)) : real(input, init);
  });

  const ada: Caller = { userId: adaId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "dashboard" };
  return { run, provided, store, cookie, boId, ada };
});

type Shop = Effect.Success<ReturnType<typeof hostedShop>>;

const authJson = (shop: Shop, path: string, init: RequestInit) =>
  Effect.promise(async () => {
    const response = await shop.run(serveAuth(new Request(`${origin}${path}`, init)));
    const text = await response.text();
    return { status: response.status, body: text === "" ? null : JSON.parse(text) };
  });

const registerAgent = Effect.fn(function* (shop: Shop) {
  const registered = yield* authJson(shop, "/api/auth/oauth2/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_name: "test agent", redirect_uris: [REDIRECT], token_endpoint_auth_method: "none",
      grant_types: ["authorization_code", "refresh_token"], response_types: ["code"],
    }),
  });
  expect(registered).toMatchObject({ status: 201, body: { application_type: "native" } });
  return Schema.decodeUnknownSync(Registered)(registered.body).client_id;
});

/** The whole connect dance Bo's agent does: authorize, consent on /connect for Shop, and trade the code for tokens. */
const connectTokens = Effect.fn(function* (shop: Shop, clientId: string) {
  const verifier = randomBytes(32).toString("base64url");
  const authorize = new URL("/api/auth/oauth2/authorize", origin);
  authorize.search = new URLSearchParams({
    response_type: "code", client_id: clientId, redirect_uri: REDIRECT, scope: "ployz offline_access", state: "state-1",
    code_challenge: createHash("sha256").update(verifier).digest("base64url"), code_challenge_method: "S256", resource: `${origin}/mcp`,
  }).toString();
  const asked = yield* Effect.promise(() => shop.run(serveAuth(new Request(authorize, { headers: { cookie: shop.cookie } }))));
  const toConnect = new URL(asked.headers.get("location") ?? expect.fail(`authorize answered ${asked.status} with no redirect`), origin);
  expect(toConnect.pathname).toBe("/connect");
  const decided = yield* shop.provided(connectAgent(new Headers({ cookie: shop.cookie }), {
    organizationId: ORGANIZATION, accept: true, oauthQuery: toConnect.search.slice(1),
  }));
  const code = new URL(decided.url).searchParams.get("code") ?? expect.fail(`consent answered ${decided.url}`);
  const issued = yield* authJson(shop, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code", code, redirect_uri: REDIRECT, client_id: clientId, code_verifier: verifier, resource: `${origin}/mcp`,
    }).toString(),
  });
  return Schema.decodeUnknownSync(Issued)(issued.body);
});

const connect = (shop: Shop, clientId: string) => connectTokens(shop, clientId).pipe(Effect.map((issued) => issued.access_token));

/** The id of Bo's one consent, the grant `/connect` made. */
const onlyConsent = Effect.fn(function* (shop: Shop) {
  const consents = yield* authJson(shop, "/api/auth/oauth2/get-consents", { method: "GET", headers: { cookie: shop.cookie } });
  const [consent] = Schema.decodeUnknownSync(Consents)(consents.body);
  return consent?.id ?? expect.fail("Bo has no consent");
});

const consentCall = (shop: Shop, path: string, body: { readonly id: string; readonly update?: { readonly scopes: ReadonlyArray<string> } }) =>
  authJson(shop, path, { method: "POST", headers: { cookie: shop.cookie, "content-type": "application/json" }, body: JSON.stringify(body) });

/** An MCP client on `/mcp` with `token`; `keys` records each response's request key header. */
const agentWith = Effect.fn(function* (shop: Shop, token: string, headers: Record<string, string> = {}) {
  const keys: Array<string | null> = [];
  const transport = new StreamableHTTPClientTransport(new URL(`${origin}/mcp`), {
    requestInit: { headers: { Authorization: `Bearer ${token}`, ...headers } },
    fetch: async (url, init) => {
      const response = await shop.run(serveRemoteMcp(new Request(url, init)));
      keys.push(response.headers.get(REQUEST_KEY_HEADER));
      return response;
    },
  });
  const client = new Client({ name: "test-agent", version: "1.0.0" });
  yield* Effect.acquireRelease(Effect.promise(() => client.connect(transport)), () => Effect.promise(() => client.close()));
  const call = (name: string, args: JsonObject) =>
    Effect.promise(() => client.callTool({ name, arguments: args })).pipe(
      Effect.map((result) => ({ isError: result.isError === true, body: Schema.decodeUnknownSync(Structured)(result.structuredContent) })),
    );
  return { client, call, keys };
});

const raw = (shop: Shop, token: string) =>
  Effect.promise(() => shop.run(serveRemoteMcp(new Request(`${origin}/mcp`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json", accept: "application/json, text/event-stream" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
  }))));

const Trusted = Schema.Struct({ approval: Schema.Unknown });

/** The approval each admit reached the Store with: `"required"` asks again, anything else lets the Deploy land. */
const admitted = (writes: { mock: { calls: ReadonlyArray<ReadonlyArray<unknown>> } }) =>
  writes.mock.calls
    .filter(([, command]) => Schema.is(Schema.Struct({ command: Schema.Literal("admit") }))(command))
    .map(([, , trusted]) => Schema.decodeUnknownSync(Trusted)(trusted).approval);

it.live("a connected agent lists the tools, reads Shop, and a refused call leaves the next one working", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const agent = yield* agentWith(shop, yield* connect(shop, yield* registerAgent(shop)));
    const { tools } = yield* Effect.promise(() => agent.client.listTools());
    expect(tools.map((tool) => tool.name)).toEqual(expect.arrayContaining(["environment_show", "service_add", "deploy"]));
    expect(tools.find((tool) => tool.name === "deploy")?.inputSchema.properties).toHaveProperty("approval");

    const shown = yield* agent.call("environment_show", SHOP);
    expect(shown).toMatchObject({ isError: false, body: { ok: true } });

    const refused = yield* agent.call("service_add", { ...SHOP, project: "nope", name: "cache", image: "redis:7" });
    expect(refused).toMatchObject({ isError: true, body: { ok: false, refusal: { code: "not_found" } } });
    expect(yield* agent.call("environment_show", SHOP)).toMatchObject({ isError: false, body: { ok: true } });
  }).pipe(Effect.scoped), 30_000);

it.live("a destroying Deploy waits on a person; an early retry only asks again, and the retry after approval deploys once", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const agent = yield* agentWith(shop, yield* connect(shop, yield* registerAgent(shop)));
    const writes = vi.spyOn(shop.store, "write");

    const asked = yield* agent.call("deploy", SHOP);
    const approvalId = asked.body.approval_required?.approval_id ?? expect.fail("the Deploy did not ask for approval");
    expect(asked.isError).toBe(true);
    expect(asked.body.approval_required?.url).toMatch(/^http:\/\/localhost:3000\/cloud\/shop\?chat=/);
    const [approval, ...others] = yield* shop.provided(pendingApprovals(ORGANIZATION));
    expect(others).toEqual([]);
    expect(approval?.id).toBe(approvalId);
    writes.mockClear();

    const early = yield* agent.call("deploy", { ...SHOP, approval: approvalId });
    expect(early.isError).toBe(true);
    expect(admitted(writes)).toEqual(["required"]);
    writes.mockClear();

    yield* shop.provided(decideApproval(shop.ada, approvalId, { approve: { digest: approval?.digest ?? "" } }));
    const approved = yield* agent.call("deploy", { ...SHOP, approval: approvalId });
    expect(approved).toMatchObject({ isError: false, body: { ok: true } });
    expect(admitted(writes)).toEqual([{ approved: approval?.digest }]);
  }).pipe(Effect.scoped), 30_000);

it.live("a keyed create replays under the same request key, which the result and the response header name", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const token = yield* connect(shop, yield* registerAgent(shop));
    const agent = yield* agentWith(shop, token, { [REQUEST_KEY_HEADER]: "add-cache-1" });
    const first = yield* agent.call("service_add", { ...SHOP, name: "cache", image: "redis:7" });
    const again = yield* agent.call("service_add", { ...SHOP, name: "cache", image: "redis:7" });
    expect(first).toMatchObject({ isError: false, body: { ok: true, request_key: "add-cache-1" } });
    expect(again).toMatchObject({ isError: false, body: { ok: true, request_key: "add-cache-1" } });
    expect(agent.keys.filter((key) => key !== null)).toEqual(["add-cache-1", "add-cache-1"]);

    const shown = yield* agent.call("environment_show", SHOP);
    expect(JSON.stringify(shown.body).match(/"name":"cache"/g)).toHaveLength(1);

    const made = yield* agent.call("project_create", { name: "docs", request_key: "make-docs-1" });
    const remade = yield* agent.call("project_create", { name: "docs", request_key: "make-docs-1" });
    expect(made).toMatchObject({ isError: false, body: { ok: true, request_key: "make-docs-1" } });
    expect(remade).toEqual(made);
    expect(agent.keys.at(-1)).toBe("make-docs-1");
  }).pipe(Effect.scoped), 30_000);

it.live("revoking the grant turns its token away, and connecting again issues one that works while the old stays refused", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const old = yield* connect(shop, clientId);
    expect((yield* raw(shop, old)).status).toBe(200);

    yield* shop.provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.delete(oauthConsent).where(sql`${oauthConsent.userId} = ${shop.boId}`);
    }));
    const revoked = yield* raw(shop, old);
    expect(revoked.status).toBe(401);
    expect(revoked.headers.get("www-authenticate")).toMatch(/^Bearer /);

    const renewed = yield* connect(shop, clientId);
    expect((yield* raw(shop, renewed)).status).toBe(200);
    expect((yield* raw(shop, old)).status).toBe(401);
  }).pipe(Effect.scoped), 30_000);

it.live("disconnecting stops the grant's refresh token, even after the agent connects again", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const old = yield* connectTokens(shop, clientId);
    const refreshToken = old.refresh_token ?? expect.fail("connect issued no refresh token");

    expect((yield* consentCall(shop, "/api/auth/oauth2/delete-consent", { id: yield* onlyConsent(shop) })).status).toBe(200);
    expect((yield* raw(shop, old.access_token)).status).toBe(401);
    expect((yield* raw(shop, yield* connect(shop, clientId))).status).toBe(200);

    const refreshed = yield* authJson(shop, "/api/auth/oauth2/token", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ grant_type: "refresh_token", client_id: clientId, refresh_token: refreshToken, resource: `${origin}/mcp` }).toString(),
    });
    expect(refreshed.status).toBe(400);
  }).pipe(Effect.scoped), 30_000);

it.live("taking the ployz scope off the consent turns the next call away", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const token = yield* connect(shop, yield* registerAgent(shop));
    expect((yield* raw(shop, token)).status).toBe(200);

    expect((yield* consentCall(shop, "/api/auth/oauth2/update-consent", { id: yield* onlyConsent(shop), update: { scopes: [] } })).status).toBe(200);
    expect((yield* raw(shop, token)).status).toBe(401);
  }).pipe(Effect.scoped), 30_000);

it.live("a self-hosted Cloud answers 404 on every remote agent route and method", () =>
  Effect.gen(function* () {
    const { run } = yield* cloudOf(false);
    const statuses = yield* Effect.promise(() => Promise.all([
      ...["GET", "POST", "DELETE", "OPTIONS"].map((method) => run(serveRemoteMcp(new Request(`${origin}/mcp`, { method })))),
      ...["/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-authorization-server", "/.well-known/oauth-authorization-server/api/auth"]
        .flatMap((path) => ["GET", "OPTIONS"].map((method) => run(serveMcpMetadata(new Request(`${origin}${path}`, { method }))))),
      ...["/api/auth/oauth2/token", "/api/auth/oauth2/register", "/api/auth/oauth2/authorize", "/api/auth/jwks"]
        .map((path) => run(serveAuth(new Request(`${origin}${path}`, { method: path === "/api/auth/oauth2/authorize" || path === "/api/auth/jwks" ? "GET" : "POST" })))),
      run(serveAuth(new Request(`${origin}/api/auth/oauth2/token`, { method: "OPTIONS" }))),
    ].map((response) => response.then((answered) => answered.status))));
    expect(new Set(statuses)).toEqual(new Set([404]));
  }).pipe(Effect.scoped), 30_000);
