import { createHash, randomBytes } from "node:crypto";
import { it } from "@effect/vitest";
import { Client, StreamableHTTPClientTransport } from "@modelcontextprotocol/client";
import { sql, type Table } from "drizzle-orm";
import type { NodePgDatabase } from "drizzle-orm/node-postgres";
import type { PgAsyncSelectBuilder, PgTable, SelectedFields } from "drizzle-orm/pg-core";
import { Effect, Fiber, Layer, Option, Schema } from "effect";
import { afterEach, expect, vi } from "vitest";
import { connectAgent, connectChoices } from "#/modules/agent/connect.server";
import { serveAuth, serveMcpMetadata, serveRemoteMcp } from "#/modules/agent/remote-mcp.server";
import { decideApproval, pendingApprovals } from "#/modules/approvals/approvals.server";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import type { Caller } from "#/modules/identity/actor";
import type { JsonObject } from "#/db/tables";
import { oauthConsent, oauthRefreshToken } from "#/modules/identity/oauth-tables";
import { member, verification } from "#/modules/identity/tables";
import { REQUEST_KEY_HEADER } from "#/modules/operations/keyed.server";
import { removeMembership } from "#/modules/organization/membership.server";
import { Auth, AuthLive } from "#/server/auth.server";
import { BetterAuthDatabase, Database } from "#/server/database.server";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const origin = "http://localhost:3000";
const REDIRECT = "http://127.0.0.1:9999/callback";
const ORGANIZATION = "00000000-0000-4000-8000-00000000a801";
const PROJECT = "00000000-0000-4000-8000-00000000a802";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a803";
const SERVICE = "00000000-0000-4000-8000-00000000a804";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000a805";
const SHOP = { project: "shop", environment: "production" };
const MCP = `${origin}/mcp`;

/** Form fields with the absent ones left out, as a client that omits a parameter sends them. */
const form = (fields: Record<string, string | null>) =>
  new URLSearchParams(Object.entries(fields).flatMap(([key, value]) => value === null ? [] : [[key, value]]));

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

type Hold = { readonly table: Table; skip: number; readonly reached: () => void; readonly wait: Promise<void> };
let held: Hold | undefined;

/** Pause here when a test holds this read of `table`, until it releases it. */
const holdOf = async (table: Table) => {
  const hold = held;
  if (hold === undefined || hold.table !== table) return;
  if (hold.skip > 0) {
    hold.skip -= 1;
    return;
  }
  held = undefined;
  hold.reached();
  await hold.wait;
};

/** The real better-auth database with every table read able to pause mid-request, so a test can interleave a disconnect. */
const holdableDatabase = (drizzle: NodePgDatabase): NodePgDatabase => {
  function select(): PgAsyncSelectBuilder<undefined>;
  function select<T extends SelectedFields>(fields: T): PgAsyncSelectBuilder<T>;
  function select(fields?: SelectedFields) {
    const builder = fields === undefined ? drizzle.select() : drizzle.select(fields);
    const from = (source: PgTable) => {
      const query = builder.from(source);
      const execute = query.execute.bind(query);
      query.execute = async () => {
        await holdOf(source);
        return execute();
      };
      return query;
    };
    return Object.create(builder, { from: { value: from } });
  }
  return Object.create(drizzle, { select: { value: select } });
};
const HoldableBetterAuthDatabase = Layer.effect(
  BetterAuthDatabase,
  Effect.map(BetterAuthDatabase, ({ drizzle }) => ({ drizzle: holdableDatabase(drizzle) })),
);

/**
 * Hold the next read of `table` after `skip` earlier ones, until `release`; `reached` resolves once it's held. Closing
 * the scope drops the hold and releases a held read, so a failed test leaves the next one a clean database.
 */
const holdRead = Effect.fn(function* (table: Table, skip = 0) {
  let reached!: () => void;
  let release!: () => void;
  const ready = new Promise<void>((resolve) => { reached = resolve; });
  const wait = new Promise<void>((resolve) => { release = resolve; });
  const hold = { table, skip, reached, wait };
  held = hold;
  yield* Effect.addFinalizer(() => Effect.sync(() => {
    if (held === hold) held = undefined;
    release();
  }));
  return { reached: Effect.promise(() => ready), release: Effect.sync(release) };
});

/** A Cloud, hosted or self-hosted, with every route the remote endpoint answers on, run as a browser or agent would. */
const cloudOf = Effect.fn(function* (hosted: boolean) {
  const cloud = yield* storeTestCloud(hosted ? { env: HOSTED_ENV, polar: hostedPolar } : {});
  const auth = AuthLive.pipe(Layer.provide(HoldableBetterAuthDatabase), Layer.provide(cloud));
  const services = yield* Layer.build(Layer.merge(auth, cloud));
  const run = Effect.runPromiseWith(services);
  const provided = <A, E>(effect: Effect.Effect<A, E, Layer.Success<typeof cloud> | Auth>) => effect.pipe(Effect.provide(services));
  return { run, provided };
});

/** Sign `email` up through the auth routes and return its session cookie. */
const signUp = (run: Effect.Success<ReturnType<typeof cloudOf>>["run"], email: string) =>
  Effect.promise(async () => {
    const response = await run(serveAuth(new Request(`${origin}/api/auth/sign-up/email`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email, name: email.split("@", 1)[0], password: "correct-horse-battery-staple" }),
    })));
    return response.headers.get("set-cookie")?.split(";", 1)[0] ?? expect.fail(`no session cookie for ${email}`);
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

  const cookie = yield* signUp(run, "bo@example.test");
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

/** Authorize for Shop, landing on the /connect page the provider signed. */
const authorize = Effect.fn(function* (shop: Shop, clientId: string, resource: string | null = MCP) {
  const verifier = randomBytes(32).toString("base64url");
  const url = new URL("/api/auth/oauth2/authorize", origin);
  url.search = form({
    response_type: "code", client_id: clientId, redirect_uri: REDIRECT, scope: "ployz offline_access", state: "state-1",
    code_challenge: createHash("sha256").update(verifier).digest("base64url"), code_challenge_method: "S256", resource,
  }).toString();
  const asked = yield* Effect.promise(() => shop.run(serveAuth(new Request(url, { headers: { cookie: shop.cookie } }))));
  const toConnect = new URL(asked.headers.get("location") ?? expect.fail(`authorize answered ${asked.status} with no redirect`), origin);
  expect(toConnect.pathname).toBe("/connect");
  return { toConnect, verifier };
});

/** Authorize and consent on /connect for Shop, leaving the code unredeemed. */
const issueCode = Effect.fn(function* (
  shop: Shop, clientId: string, pageQuery: (signed: string) => string = (signed) => signed, resource: string | null = MCP,
) {
  const { toConnect, verifier } = yield* authorize(shop, clientId, resource);
  const decided = yield* shop.provided(connectAgent(new Headers({ cookie: shop.cookie }), {
    organizationId: ORGANIZATION, accept: true, oauthQuery: pageQuery(toConnect.search.slice(1)),
  }));
  const code = new URL(decided.url).searchParams.get("code") ?? expect.fail(`consent answered ${decided.url}`);
  return { clientId, code, verifier };
});

const redeem = (shop: Shop, { clientId, code, verifier }: Effect.Success<ReturnType<typeof issueCode>>, resource: string | null = MCP) =>
  authJson(shop, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: form({ grant_type: "authorization_code", code, redirect_uri: REDIRECT, client_id: clientId, code_verifier: verifier, resource }).toString(),
  });

const refresh = (shop: Shop, clientId: string, refreshToken: string, resource: string | null = MCP) =>
  authJson(shop, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: form({ grant_type: "refresh_token", client_id: clientId, refresh_token: refreshToken, resource }).toString(),
  });

/** The whole connect dance Bo's agent does: authorize, consent on /connect for Shop, and trade the code for tokens. */
const connectTokens = Effect.fn(function* (shop: Shop, clientId: string, pageQuery?: (signed: string) => string) {
  const issued = yield* redeem(shop, yield* issueCode(shop, clientId, pageQuery));
  return Schema.decodeUnknownSync(Issued)(issued.body);
});

const connect = (shop: Shop, clientId: string) => connectTokens(shop, clientId).pipe(Effect.map((issued) => issued.access_token));

/** The id of Bo's one consent, the grant `/connect` made. */
const onlyConsent = Effect.fn(function* (shop: Shop) {
  const consents = yield* authJson(shop, "/api/auth/oauth2/get-consents", { method: "GET", headers: { cookie: shop.cookie } });
  const [consent] = Schema.decodeUnknownSync(Consents)(consents.body);
  return consent?.id ?? expect.fail("Bo has no consent");
});

const consentCall = (shop: Shop, path: string, body: { readonly id: string; readonly update?: { readonly scopes: ReadonlyArray<string> } }, cookie = shop.cookie) =>
  authJson(shop, path, { method: "POST", headers: { cookie, "content-type": "application/json" }, body: JSON.stringify(body) });

const disconnect = (shop: Shop) => Effect.flatMap(onlyConsent(shop), (id) => consentCall(shop, "/api/auth/oauth2/delete-consent", { id }));

/** Codes, refresh tokens and consents are stamped in whole seconds, so a reconnect that must count as later waits one out. */
const aSecondLater = Effect.sleep("1100 millis");

/** A refresh token row as better-auth stores one, made outside the token endpoint. */
const plantRefreshToken = Effect.fn(function* (shop: Shop, clientId: string) {
  const token = randomBytes(32).toString("base64url");
  yield* shop.provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.insert(oauthRefreshToken).values({
      token: createHash("sha256").update(token).digest("base64url"), clientId, userId: shop.boId, referenceId: ORGANIZATION,
      resources: [MCP], scopes: ["ployz", "offline_access"], authTime: new Date(), expiresAt: new Date(Date.now() + 86_400_000),
    });
  }));
  return token;
});

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

it.live("leaving Shop ends the grant, and neither rejoining nor connecting again revives its credentials", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const old = yield* connectTokens(shop, clientId);
    const oldRefreshToken = old.refresh_token ?? expect.fail("no refresh token was issued");
    expect((yield* raw(shop, old.access_token)).status).toBe(200);

    yield* shop.provided(removeMembership(ORGANIZATION, shop.boId, shop.boId));
    expect((yield* raw(shop, old.access_token)).status).toBe(401);
    yield* aSecondLater;
    yield* shop.provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(member).values({ userId: shop.boId, organizationId: ORGANIZATION, role: "owner" });
    }));
    expect((yield* raw(shop, old.access_token)).status).toBe(401);

    const renewed = yield* connect(shop, clientId);
    expect((yield* raw(shop, renewed)).status).toBe(200);
    expect((yield* raw(shop, old.access_token)).status).toBe(401);
    expect(yield* refresh(shop, clientId, oldRefreshToken))
      .toEqual({ status: 400, body: { error: "invalid_grant", error_description: "the connection was revoked" } });
  }).pipe(Effect.scoped), 30_000);

it.live("a membership that ends outside removeMembership leaves a consent that Allow replaces after rejoining", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const old = yield* connectTokens(shop, clientId);
    const oldRefreshToken = old.refresh_token ?? expect.fail("no refresh token was issued");

    yield* shop.provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.delete(member).where(sql`${member.userId} = ${shop.boId}`);
    }));
    yield* aSecondLater;
    yield* shop.provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(member).values({ userId: shop.boId, organizationId: ORGANIZATION, role: "owner" });
    }));

    const renewed = yield* connect(shop, clientId);
    expect((yield* raw(shop, renewed)).status).toBe(200);
    expect((yield* raw(shop, old.access_token)).status).toBe(401);
    expect(yield* refresh(shop, clientId, oldRefreshToken))
      .toEqual({ status: 400, body: { error: "invalid_grant", error_description: "the connection was revoked" } });
  }).pipe(Effect.scoped), 30_000);

it.live("/connect names the agent asking and the host its code returns to, and names none for a tampered link", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const { toConnect } = yield* authorize(shop, yield* registerAgent(shop));
    const choices = (pageQuery: string) => shop.provided(connectChoices(new Headers({ cookie: shop.cookie }), pageQuery));
    const signed = toConnect.search.slice(1);
    expect((yield* choices(signed))?.agent).toEqual({ name: "test agent", host: "127.0.0.1:9999" });

    const tampered = new URLSearchParams(signed);
    tampered.set("redirect_uri", "https://attacker.example/callback");
    expect((yield* choices(tampered.toString()))?.agent).toBeNull();
  }).pipe(Effect.scoped), 30_000);

it.live("a parameter the browser adds to /connect leaves the consent standing", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const issued = yield* connectTokens(shop, yield* registerAgent(shop), (signed) => `${signed}&utm_source=newsletter`);
    expect((yield* raw(shop, issued.access_token)).status).toBe(200);
  }).pipe(Effect.scoped), 30_000);

it.live("disconnecting stops the grant's refresh token, even after the agent connects again", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const old = yield* connectTokens(shop, clientId);
    const refreshToken = old.refresh_token ?? expect.fail("connect issued no refresh token");

    expect((yield* disconnect(shop)).status).toBe(200);
    expect((yield* raw(shop, old.access_token)).status).toBe(401);
    yield* aSecondLater;
    expect((yield* raw(shop, yield* connect(shop, clientId))).status).toBe(200);

    expect((yield* refresh(shop, clientId, refreshToken)).status).toBe(400);
  }).pipe(Effect.scoped), 30_000);

it.live("disconnecting stops a code the agent hasn't redeemed, even after the agent connects again", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    yield* connectTokens(shop, clientId);
    const pending = yield* issueCode(shop, clientId);

    expect((yield* disconnect(shop)).status).toBe(200);
    yield* aSecondLater;
    expect((yield* raw(shop, yield* connect(shop, clientId))).status).toBe(200);

    expect((yield* redeem(shop, pending)).status).toBe(400);
  }).pipe(Effect.scoped), 30_000);

it.live("disconnecting stops every code the agent hasn't redeemed, however many it holds", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    yield* connectTokens(shop, clientId);
    const pending = yield* Effect.forEach(Array.from({ length: 101 }), () => issueCode(shop, clientId));

    expect((yield* disconnect(shop)).status).toBe(200);
    yield* aSecondLater;
    expect((yield* raw(shop, yield* connect(shop, clientId))).status).toBe(200);

    const statuses = yield* Effect.forEach(pending, (code) => redeem(shop, code).pipe(Effect.map((answer) => answer.status)));
    expect(new Set(statuses)).toEqual(new Set([400]));
  }).pipe(Effect.scoped), 60_000);

it.live("a refresh token made while disconnected mints nothing after the agent connects again", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    yield* connectTokens(shop, clientId);

    expect((yield* disconnect(shop)).status).toBe(200);
    const late = yield* plantRefreshToken(shop, clientId);
    yield* aSecondLater;
    const renewed = yield* connectTokens(shop, clientId);
    expect((yield* raw(shop, renewed.access_token)).status).toBe(200);

    expect((yield* refresh(shop, clientId, late)).status).toBe(400);
    expect((yield* refresh(shop, clientId, renewed.refresh_token ?? expect.fail("reconnect issued no refresh token"))).status).toBe(200);
  }).pipe(Effect.scoped), 30_000);

it.live("someone else's refused disconnect doesn't keep Bo's old credentials alive", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const old = yield* connectTokens(shop, clientId);
    const refreshToken = old.refresh_token ?? expect.fail("connect issued no refresh token");
    const pending = yield* issueCode(shop, clientId);
    const consentId = yield* onlyConsent(shop);
    const outsider = yield* signUp(shop.run, "outsider@example.test");

    expect((yield* consentCall(shop, "/api/auth/oauth2/delete-consent", { id: consentId }, outsider)).status).toBe(401);
    expect((yield* disconnect(shop)).status).toBe(200);
    yield* aSecondLater;
    expect((yield* raw(shop, yield* connect(shop, clientId))).status).toBe(200);

    expect((yield* redeem(shop, pending)).status).toBe(400);
    expect((yield* refresh(shop, clientId, refreshToken)).status).toBe(400);
  }).pipe(Effect.scoped), 30_000);

it.live("a code redeemed while disconnected yields no refresh token that works after the agent connects again", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    yield* connectTokens(shop, clientId);
    const pending = yield* issueCode(shop, clientId);

    expect((yield* disconnect(shop)).status).toBe(200);
    const redeemed = yield* redeem(shop, pending);
    const minted = redeemed.status === 200 ? Schema.decodeUnknownSync(Issued)(redeemed.body).refresh_token : undefined;
    expect((yield* raw(shop, yield* connect(shop, clientId))).status).toBe(200);

    if (minted !== undefined) expect((yield* refresh(shop, clientId, minted)).status).toBe(400);
    expect(redeemed.status).toBe(400);
  }).pipe(Effect.scoped), 30_000);

/**
 * A token request that passed the hook's check but hasn't consumed its credential yet, held at the provider's own read
 * of it, with Bo disconnecting and connecting again meanwhile. The hook's own read of the credential row comes first,
 * so the held read is the second one on that table.
 */
const resumedAfterReconnect = Effect.fn(function* (shop: Shop, clientId: string, kind: "code" | "refresh") {
  const old = yield* connectTokens(shop, clientId);
  const pending = yield* issueCode(shop, clientId);
  yield* aSecondLater;
  const request = kind === "code"
    ? { hold: yield* holdRead(verification, 1), send: redeem(shop, pending) }
    : { hold: yield* holdRead(oauthRefreshToken, 1), send: refresh(shop, clientId, old.refresh_token ?? expect.fail("connect issued no refresh token")) };
  const inFlight = yield* Effect.forkScoped(request.send);
  yield* request.hold.reached;

  const renewed = yield* Effect.gen(function* () {
    expect((yield* disconnect(shop)).status).toBe(200);
    yield* aSecondLater;
    return yield* connectTokens(shop, clientId);
  }).pipe(Effect.ensuring(request.hold.release));
  const answered = yield* Fiber.join(inFlight);
  expect((yield* raw(shop, renewed.access_token)).status).toBe(200);
  return answered;
});

it.live("a redemption held after its check, resumed after a disconnect and reconnect, mints nothing", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const answered = yield* resumedAfterReconnect(shop, yield* registerAgent(shop), "code");
    expect(answered).toEqual({ status: 400, body: { error: "invalid_grant", error_description: "the connection was revoked" } });
  }).pipe(Effect.scoped), 30_000);

it.live("a refresh held after its check, resumed after a disconnect and reconnect, mints nothing", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const answered = yield* resumedAfterReconnect(shop, yield* registerAgent(shop), "refresh");
    expect(answered).toEqual({ status: 400, body: { error: "invalid_grant", error_description: "the connection was revoked" } });
  }).pipe(Effect.scoped), 30_000);

it.live("a redemption without a resource, held and resumed after a disconnect and reconnect, leaves nothing that reaches /mcp", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const pending = yield* issueCode(shop, clientId, undefined, null);
    yield* aSecondLater;
    const hold = yield* holdRead(verification, 1);
    const inFlight = yield* Effect.forkScoped(redeem(shop, pending, null));
    // A request refused before the provider reads its code never reaches the hold, and has nothing left to resume.
    const paused = yield* Effect.raceFirst(Effect.as(hold.reached, true), Effect.as(Fiber.await(inFlight), false));
    held = undefined;
    if (paused) {
      yield* Effect.gen(function* () {
        expect((yield* disconnect(shop)).status).toBe(200);
        yield* aSecondLater;
        const renewed = yield* redeem(shop, yield* issueCode(shop, clientId, undefined, null));
        expect((yield* raw(shop, Schema.decodeUnknownSync(Issued)(renewed.body).access_token)).status).toBe(200);
      }).pipe(Effect.ensuring(hold.release));
    }
    const answered = yield* Fiber.join(inFlight);
    const issued = Schema.decodeUnknownOption(Issued)(answered.body);
    if (Option.isSome(issued)) {
      expect((yield* raw(shop, issued.value.access_token)).status).toBe(401);
      const renewable = issued.value.refresh_token;
      if (renewable !== undefined) expect((yield* refresh(shop, clientId, renewable)).status).toBe(400);
    }
    expect(answered.status).toBe(400);
  }).pipe(Effect.scoped), 30_000);

it.live("a token request without a resource is refused, for a code, a refresh token and a padded grant type", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    const clientId = yield* registerAgent(shop);
    const issued = yield* connectTokens(shop, clientId);
    const refreshToken = issued.refresh_token ?? expect.fail("connect issued no refresh token");
    const unaddressed = { status: 400, body: { error: "invalid_target", error_description: `a token is only issued for ${MCP}` } };
    expect(yield* redeem(shop, yield* issueCode(shop, clientId), null)).toEqual(unaddressed);
    expect(yield* redeem(shop, yield* issueCode(shop, clientId, undefined, null), null)).toEqual(unaddressed);
    expect(yield* refresh(shop, clientId, refreshToken, null)).toEqual(unaddressed);
    const padded = yield* issueCode(shop, clientId, undefined, null);
    expect(yield* authJson(shop, "/api/auth/oauth2/token", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: form({ grant_type: " authorization_code ", code: padded.code, redirect_uri: REDIRECT, client_id: clientId, code_verifier: padded.verifier }).toString(),
    })).toEqual(unaddressed);
    expect((yield* refresh(shop, clientId, refreshToken)).status).toBe(200);
  }).pipe(Effect.scoped), 30_000);

it.live("disconnecting a consent id that isn't one answers 404", () =>
  Effect.gen(function* () {
    const shop = yield* hostedShop();
    for (const id of ["", "not-a-consent"]) {
      expect((yield* consentCall(shop, "/api/auth/oauth2/delete-consent", { id })).status).toBe(404);
    }
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
