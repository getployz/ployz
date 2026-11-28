import { testConfigEnvironment } from "#/test/config-environment";
import { createHash, randomUUID } from "node:crypto";
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createClientRpc } from "@tanstack/react-start/client-rpc";
import { runWithStartContext } from "@tanstack/start-storage-context";
import { ConfigProvider, Effect, Exit, Layer, Schema, Scope } from "effect";
import { Inngest } from "inngest";
import { Client } from "pg";
import { createServer } from "vite";
import { afterAll, beforeAll, expect, it } from "vitest";
import { CloudStoreLive, cloudStore } from "#/modules/config-store/store-sdk.server";
import { Auth, AuthLive } from "#/modules/identity/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import { postgresTestDatabase } from "#/test/postgres";
import { makeSecretEncryption } from "#/utils/encrypted-secret.server";

const configFile = fileURLToPath(new URL("../../vite.config.ts", import.meta.url));
const BoundaryResult = Schema.Union([
  Schema.Struct({ userId: Schema.String }),
  Schema.Struct({ disposed: Schema.Literal(true) }),
]);
const decodeBoundaryResult = Schema.decodeUnknownSync(BoundaryResult);
const GithubWebResult = Schema.Struct({
  origin: Schema.String,
});
const decodeGithubWebResult = Schema.decodeUnknownSync(GithubWebResult);

type BoundaryData = {
  readonly action: "actor" | "fail" | "dispose";
  readonly unexpected?: boolean;
};

const BROWSER = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const SSR_DEPLOYMENT = "00000000-0000-4000-8000-0000000c0de1";
const SSR_SERVICE = "00000000-0000-4000-8000-0000000ca5e1";

/**
 * Pages a cold load must keep cheap: at most one session lookup, and content (`shows`) that never waits on the Cluster.
 */
const pages = {
  // A Service's card on the canvas.
  environment: { path: "ssr-project/ssr-production", shows: `data-canvas-node="${SSR_SERVICE}"` },
  // The Run's progress.
  deployment: { path: `ssr-project/ssr-production/deployments/${SSR_DEPLOYMENT}`, shows: " · 0 of 1" },
  // The Service drawer's Scale.
  service: { path: `ssr-project/ssr-production/services/${SSR_SERVICE}`, shows: 'aria-label="Replicas' },
  // The Environment's region.
  settings: { path: "ssr-project/ssr-production/settings", shows: `id="settings-region"` },
};

/**
 * Counts better-auth's reads of a session by its token, from any pool: the dev server's SSR runs in its own Vite
 * environment, out of reach of this process's `pg`. Only sessions opened after this call are tracked.
 */
async function countSessionLookups(url: URL) {
  const counter = new Client({ connectionString: url.href });
  await counter.connect();
  const { rows: [database] } = await counter.query<{ id: number }>("select oid::int as id from pg_database where datname = current_database()");
  await counter.query("create extension if not exists pg_stat_statements");
  await counter.query(`alter database ${counter.escapeIdentifier(counter.database ?? "")} set pg_stat_statements.track = 'top'`);
  return {
    async during(run: () => Promise<void>) {
      await counter.query("select pg_stat_statements_reset(0, $1, 0)", [database?.id]);
      await run();
      const { rows } = await counter.query<{ calls: number }>(`select coalesce(sum(calls), 0)::int as calls from pg_stat_statements
        where dbid = $1 and query like '%from "session" where "session"."token"%'`, [database?.id]);
      return rows[0]?.calls ?? 0;
    },
    end: () => counter.end(),
  };
}

/**
 * Pairs the Organization with one Server, then holds its Cluster: the Cluster's first read of its Servers waits on a lock
 * until `release`, as a Cluster that never answers would. A page that waits on its Cluster sends no byte while it is held.
 */
async function holdCluster(url: URL, organizationId: string) {
  const holder = new Client({ connectionString: url.href });
  await holder.connect();
  const encryption = makeSecretEncryption(process.env["APP_ENCRYPTION_SECRET"] ?? "");
  const pairing = "cold-load-pairing";
  const machineId = "00000000000000000000000000008911";
  await holder.query(`insert into organization_pairing
    (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id, founder_machine_id)
    values ($1, $2, 'founder-key', $3, $3)`, [organizationId, encryption.encrypt(pairing), machineId]);
  await holder.query(`insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, is_dial_entry)
    values ($1, $2, $3, $4, true)`, [organizationId, machineId, createHash("sha256").update(pairing).digest("hex"), encryption.encrypt("ployz1:cold-load")]);
  return {
    async during<A>(run: () => Promise<A>) {
      await holder.query("begin");
      await holder.query("lock table organization_machine in access exclusive mode");
      try {
        return await run();
      } finally {
        await holder.query("rollback");
      }
    },
    end: () => holder.end(),
  };
}

const PASSWORD = "correct-horse-battery-staple";

/** One signed-in Cloud on a test database, served by a Vite dev server, shared by every test below. */
async function startCloud() {
  const scope = Scope.makeUnsafe();
  const run = <A>(effect: Effect.Effect<A, never, Scope.Scope>) => Effect.runPromise(effect.pipe(Scope.provide(scope)));
  const testDatabase = await run(postgresTestDatabase);
  const provider = ConfigProvider.fromEnv({
    env: { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: testDatabase.url.href },
  });
  const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(provider)));
  const databaseLayer = DatabaseLive.pipe(Layer.provide(configLayer));
  const authLayer = AuthLive.pipe(Layer.provide(Layer.mergeAll(
    configLayer,
    databaseLayer,
    Layer.succeed(Polar, { mode: "self_hosted" }),
    Layer.succeed(InngestClient, new Inngest({ id: "tanstack-boundary-test" })),
  )));
  const signIn = (path: "sign-up" | "sign-in") => Effect.gen(function* () {
    const auth = yield* Auth;
    const response = yield* auth.handler(new Request(`http://localhost:3000/api/auth/${path}/email`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email: "boundary@example.test", name: "Boundary", password: PASSWORD }),
    }));
    const value = response.headers.get("set-cookie")?.split(";", 1)[0];
    if (value === undefined) return yield* Effect.die("Better Auth did not set a session cookie");
    return value;
  });
  const [cookie, laterRevoked] = await Effect.runPromise(Effect.all([signIn("sign-up"), signIn("sign-in")]).pipe(Effect.provide(authLayer)));

  const organization = await (async () => {
    const database = new Client({ connectionString: testDatabase.url.href });
    await database.connect();
    try {
      const { rows: [row] } = await database.query<{ id: string; slug: string }>("select id, slug from organization");
      if (!row) throw new Error("Signup did not create an organization.");
      return row;
    } finally {
      await database.end();
    }
  })();
  await Effect.runPromise(Effect.gen(function* () {
    const store = yield* cloudStore;
    yield* Effect.promise(async () => {
      const environment = { project: "ssr-project", environment: "ssr-production" };
      await store.write(organization.id, { command: "create_project", id: randomUUID(), name: "ssr-project", default_environment: randomUUID() });
      await store.write(organization.id, { command: "create_environment", id: randomUUID(), project: "ssr-project", name: "ssr-production" });
      await store.write(organization.id, { command: "create_service", id: SSR_SERVICE, environment, name: "cache", image: "redis:7" });
      await store.write(organization.id, {
        command: "admit", admit: "deploy", id: SSR_DEPLOYMENT, environment, services: [], version: null, accept_volume_loss: [],
      });
    });
  }).pipe(Effect.provide(CloudStoreLive.pipe(Layer.provide(Layer.merge(configLayer, databaseLayer))))));

  const previousEnv = Object.fromEntries(["DATABASE_URL", "BETTER_AUTH_SECRET", "TSS_SERVER_FN_BASE"].map((name) => [name, process.env[name]]));
  await run(Effect.addFinalizer(() => Effect.sync(() => {
    for (const [name, value] of Object.entries(previousEnv)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  })));
  process.env["DATABASE_URL"] = testDatabase.url.href;
  process.env["BETTER_AUTH_SECRET"] = "better-auth-secret";
  process.env["TSS_SERVER_FN_BASE"] = "/_serverFn/";

  const sessions = await countSessionLookups(testDatabase.url);
  await run(Effect.addFinalizer(() => Effect.promise(() => sessions.end())));
  const cluster = await holdCluster(testDatabase.url, organization.id);
  await run(Effect.addFinalizer(() => Effect.promise(() => cluster.end())));
  const vite = await createServer({
    configFile,
    // Keep concurrent cold SSR imports in flight long enough to expose partial exports.
    // Vite <8.2.2 mistakes completed dependencies for an active import cycle (#23009).
    plugins: [{
      name: "delay-cold-server-function",
      enforce: "post",
      transform(code, id) {
        if (id.includes("/collections/read.functions.ts?tss-serverfn-split")) {
          return "await new Promise(resolve => setTimeout(resolve, 100));\n" + code;
        }
      },
    }],
    optimizeDeps: { noDiscovery: true },
    server: {
      host: "127.0.0.1",
      port: 0,
      strictPort: false,
      fs: { allow: [process.cwd(), realpathSync(fileURLToPath(new URL("../../node_modules", import.meta.url)))] },
    },
  });
  await run(Effect.addFinalizer(() => Effect.promise(() => vite.close())));
  await vite.listen();
  const origin = vite.resolvedUrls?.local[0];
  if (origin === undefined) throw new Error("Vite did not expose its URL.");

  const serverFnIdOf = async (file: string, pattern: RegExp) => {
    const id = pattern.exec((await vite.transformRequest(file))?.code ?? "")?.[1];
    if (id === undefined) throw new Error(`TanStack did not compile the server function in ${file}.`);
    return id;
  };
  const rpc = createClientRpc(await serverFnIdOf("/src/test/fixtures/effect-boundary.functions.ts", /createClientRpc\("([^"]+)"\)/u));
  const githubWebRpc = createClientRpc(await serverFnIdOf("/src/modules/github/github.functions.ts",
    /getGithubWebServerFn\s*=[^\n]*?createClientRpc\("([^"]+)"\)/u));
  const baseFetch: typeof fetch = (input, init) => fetch(new URL(input instanceof Request ? input.url : String(input), origin), init);
  const inStartContext = <A>(call: () => Promise<A>) => runWithStartContext({
    getRouter: () => {
      throw new Error("The client RPC test does not use a router.");
    },
    request: new Request(origin),
    startOptions: {},
    contextAfterGlobalMiddlewares: {},
    executedRequestMiddlewares: new Set(),
    handlerType: "serverFn",
  }, call);
  const invoke = async (data: BoundaryData, signal?: AbortSignal) => {
    const frame = await inStartContext(() => rpc({
      method: "POST", data, signal, headers: { cookie, "sec-fetch-site": "same-origin" }, fetch: baseFetch,
    }));
    if (frame.error) throw frame.error;
    return decodeBoundaryResult(frame.result, { onExcessProperty: "error" });
  };
  const invokeGithubWeb = async (authenticated: boolean) => {
    const frame = await inStartContext(() => githubWebRpc({
      method: "GET",
      data: undefined,
      headers: authenticated ? { cookie, "sec-fetch-site": "same-origin" } : { "sec-fetch-site": "same-origin" },
      fetch: baseFetch,
    }));
    if (frame.error) throw frame.error;
    return decodeGithubWebResult(frame.result, { onExcessProperty: "error" });
  };
  // Disposes the app runtime before the dev server closes; finalizers run in reverse.
  await run(Effect.addFinalizer(() => Effect.promise(() => invoke({ action: "dispose" })).pipe(Effect.ignore)));

  // A browser's user agent: TanStack renders the whole page before its first byte for a bot, which Node's fetch reads as.
  const page = (path: string, init: RequestInit & { cookie?: string } = {}) => {
    const headers = new Headers({ "user-agent": BROWSER });
    if (init.cookie !== undefined) headers.set("cookie", init.cookie);
    return fetch(new URL(`/cloud/${organization.slug}/${path}`, origin), { redirect: "manual", ...init, headers });
  };
  // Compiles the app's modules first. The Projects page asks no Cluster, so the Cluster checks below start unobserved.
  await (await page("~", { cookie })).text();

  return {
    testDatabase, sessions, cluster, cookie, laterRevoked, page, invoke, invokeGithubWeb,
    stop: () => Effect.runPromise(Scope.close(scope, Exit.void)),
  };
}

let cloud: Awaited<ReturnType<typeof startCloud>>;
beforeAll(async () => {
  cloud = await startCloud();
}, 180_000);
afterAll(() => cloud?.stop());

/** The page's HTML up to and including `shows`, or all it sent before the stream ended or `signal` aborted. */
async function readUntil(response: Response, shows: string) {
  const reader = response.body?.getReader();
  const decoder = new TextDecoder();
  let html = "";
  try {
    while (reader && !html.includes(shows)) {
      const { done, value } = await reader.read();
      if (done) break;
      html += decoder.decode(value, { stream: true });
    }
  } catch {
    // Aborted: the page held back what it had not sent.
  } finally {
    void reader?.cancel().catch(() => {});
  }
  return html;
}

it.each(Object.entries(pages))("the %s page shows its content while its Cluster doesn't answer", async (_name, { path, shows }) => {
  const html = await cloud.cluster.during(async () => {
    const response = await cloud.page(path, { cookie: cloud.cookie, signal: AbortSignal.timeout(20_000) });
    expect(response.status).toBe(200);
    return readUntil(response, shows);
  });
  expect(html).toContain(shows);
}, 60_000);

it.each(Object.entries(pages))("a cold load of the %s page looks up its session at most once", async (_name, { path }) => {
  const lookups = await cloud.sessions.during(async () => {
    const response = await cloud.page(path, { cookie: cloud.cookie });
    expect(response.status).toBe(200);
    await response.text();
  });
  expect(lookups).toBeLessThanOrEqual(1);
}, 60_000);

it("renders the Environment page on the server inside the dashboard shell", async () => {
  const response = await cloud.page(pages.environment.path, { cookie: cloud.cookie });
  const html = await response.text();
  expect(response.status).toBe(200);
  expect(html).toContain("ssr-project");
  expect(html).toContain("ssr-production");
  expect(html).toContain('aria-label="Dashboard navigation"');
  expect(html).toContain("dehydratedDbClient");
  expect(html).not.toContain("Switched to client rendering");
});

it("sends an anonymous request to sign in, and refuses a revoked session on its next request", async () => {
  const anonymous = await cloud.page(pages.environment.path);
  expect(anonymous.status).toBe(307);
  expect(anonymous.headers.get("location")).toBe(`/auth?callbackURL=${encodeURIComponent(`/cloud/boundary/${pages.environment.path}`)}`);

  expect((await cloud.page(pages.environment.path, { cookie: cloud.laterRevoked })).status).toBe(200);
  const token = decodeURIComponent(cloud.laterRevoked.slice(cloud.laterRevoked.indexOf("=") + 1)).split(".", 1)[0];
  const database = new Client({ connectionString: cloud.testDatabase.url.href });
  await database.connect();
  try {
    expect((await database.query("delete from session where token = $1", [token])).rowCount).toBe(1);
  } finally {
    await database.end();
  }
  const afterRevocation = await cloud.page(pages.environment.path, { cookie: cloud.laterRevoked });
  expect(afterRevocation.status).toBe(307);
  expect(afterRevocation.headers.get("location")).toBe(`/auth?callbackURL=${encodeURIComponent(`/cloud/boundary/${pages.environment.path}`)}`);
});

it("round-trips strict validation, the Actor, and redacted errors through TanStack", async () => {
  await expect(cloud.invoke({ action: "actor" })).resolves.toEqual({ userId: expect.any(String) });
  await expect(cloud.invoke({ action: "actor", unexpected: true })).rejects.toEqual({
    _tag: "PublicError", code: "VALIDATION_FAILED", message: "The request is invalid.",
  });
  try {
    await cloud.invoke({ action: "fail" });
    expect.unreachable("Expected the Effect program to fail");
  } catch (cause) {
    expect(cause).toEqual({ _tag: "PublicError", code: "INTERNAL", message: "The request could not be completed." });
    expect(cause).not.toBeInstanceOf(Error);
    expect(JSON.stringify(cause)).not.toMatch(/application-secret|provider-token|db\.internal|stack|cause/u);
  }
  await expect(cloud.invokeGithubWeb(false)).rejects.toEqual({
    _tag: "PublicError", code: "UNAUTHORIZED", message: "Authentication is required.",
  });
  await expect(cloud.invokeGithubWeb(true)).resolves.toEqual({ origin: "https://github.com" });
});
