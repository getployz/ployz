import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { createHash } from "node:crypto";
import type { Client } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Cause, ConfigProvider, Effect, Exit, Layer } from "effect";
import { Inngest } from "inngest";
import { organizationBillingState } from "#/modules/billing/tables";
import { Polar, type PolarService } from "#/modules/billing/polar-provider.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { githubInstallation, githubRepositoryCache } from "#/modules/github/tables";
import { InngestClient } from "#/modules/inngest/client";
import { member, organizationToken, session } from "#/modules/identity/tables";
import { asTestDouble } from "#/lib/test-double";
import { retireServerAccess, serverAccessLabel } from "#/modules/machines/server-access.server";
import { organizationMachine, serverAccess } from "#/modules/machines/tables";
import { makePloyzLayer, Ployz } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import { fakeGithubApi } from "#/test/fake-github";
import { postgresTestDatabase } from "#/test/postgres";

const origin = "http://localhost:3000";

const hostedPolar: PolarService = {
  mode: "hosted",
  productId: "pro",
  listActiveSubscriptions: () => Effect.die("billing reads the cached row"),
  createCheckout: () => Effect.succeed({ url: "https://polar.test/checkout" }),
  createCustomerPortal: () => Effect.succeed({ customerPortalUrl: "https://polar.test/portal" }),
};

/** GitHub: the private acme/web (through installation 7) has branches main and dev. */
const github = fakeGithubApi({
  "https://api.github.com/repos/acme/web/branches?per_page=100&page=1": [{ name: "main" }, { name: "dev" }],
});

const encryption = makeSecretEncryption("fixture-server-access-encryption-1234567890");

/** Fake Servers keyed by Machine ID: each holds its Management Client slots, and can go offline. */
function fakeServers() {
  const servers = new Map<string, { online: boolean; slots: Map<string, string> }>();
  let sets = 0;
  const layer = makePloyzLayer({
    connect: async (options) => {
      const [connection] = options.connections;
      const server = connection && "machine_id" in connection ? servers.get(connection.machine_id ?? "") : undefined;
      if (server === undefined || !server.online) throw new Error("Endpoint unavailable");
      return asTestDouble<Client>()({
        setManagementClient: async (label: string) => {
          sets += 1;
          const capability = `ployz1:${connection?.machine_id}:${label}:${sets}`;
          server.slots.set(label, capability);
          return capability;
        },
        clearManagementClient: async (label: string) => { server.slots.delete(label); },
        close: async () => undefined,
      });
    },
  });
  return { servers, layer, sets: () => sets };
}

const pairingSecret = "ppair_fixture_server_access";

/** Pair the Organization with Cloud and enroll one Server. */
const enroll = Effect.fn(function* (organizationId: string, machineId: string, first: boolean) {
  const database = yield* Database;
  if (first) {
    yield* database.drizzle.insert(organizationPairing).values({
      organizationId,
      encryptedPairingSecret: encryption.encrypt(pairingSecret),
      founderPublicKey: "founder-key",
      founderClaimMachineId: machineId,
    });
  }
  yield* database.drizzle.insert(organizationMachine).values({
    organizationId,
    machineId,
    clusterKey: createHash("sha256").update(pairingSecret).digest("hex"),
    encryptedCapability: encryption.encrypt(`ployz1:cloud:${machineId}`),
    isDialEntry: first,
  });
});

const cliLayer = Effect.fn(function* (polar: PolarService, nodeEnv = "test", ployz: Layer.Layer<Ployz> = fakeServers().layer) {
  const testDatabase = yield* postgresTestDatabase;
  const provider = ConfigProvider.fromEnv({
    env: { ...testConfigEnvironment(), NODE_ENV: nodeEnv, DATABASE_URL: testDatabase.url.href },
  });
  const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(provider)));
  const databaseLayer = DatabaseLive.pipe(Layer.provide(configLayer));
  const services = Layer.mergeAll(
    configLayer,
    databaseLayer,
    Layer.succeed(Polar, polar),
    Layer.succeed(InngestClient, new Inngest({ id: "cli-test" })),
    Layer.succeed(SecretEncryption, encryption),
    Layer.succeed(GithubApi, github.service),
    ployz,
  );
  return Layer.merge(AuthLive.pipe(Layer.provide(services)), services);
});

/** The fields these tests read from `/api/cli` replies. */
type Reply = {
  readonly organizations?: ReadonlyArray<{ readonly id: string; readonly slug: string; readonly current: boolean }>;
  readonly token?: { readonly id: string; readonly secret: string; readonly organization: string };
  readonly tokens?: ReadonlyArray<{ readonly id: string; readonly current: boolean; readonly expired: boolean }>;
  readonly devices?: ReadonlyArray<{ readonly id: string; readonly current: boolean }>;
  readonly removed?: { readonly id: string; readonly kind: string };
  readonly billing?: { readonly self_hosted: boolean; readonly pro: boolean; readonly custom_domains: boolean };
  readonly url?: string;
  readonly connections?: ReadonlyArray<{ readonly machine_id: string; readonly management: string }>;
  readonly unreachable?: ReadonlyArray<string>;
  readonly servers?: { readonly confirmed: ReadonlyArray<string>; readonly unconfirmed: ReadonlyArray<string> };
  readonly signed_out?: { readonly id: string };
  readonly linked?: boolean;
  readonly ready?: boolean;
  readonly install_url?: string;
  readonly installations?: ReadonlyArray<{ readonly id: number; readonly account: string; readonly repositories: number }>;
  readonly repositories?: ReadonlyArray<{ readonly repository: string; readonly installation: number }>;
  readonly branches?: ReadonlyArray<string>;
  readonly access?: string;
  readonly disconnected?: { readonly id: number; readonly account: string };
  readonly uninstall_url?: string;
  readonly revoking?: ReadonlyArray<{ readonly id: string; readonly kind: string; readonly unconfirmed: ReadonlyArray<string> }>;
};

type As = { readonly cookie?: string; readonly bearer?: string };

const cli = Effect.fn(function* (method: string, path: string, as: As, body?: Readonly<Record<string, string | number>>) {
  const headers = new Headers();
  if (as.cookie !== undefined) headers.set("cookie", as.cookie);
  if (as.bearer !== undefined) headers.set("authorization", `Bearer ${as.bearer}`);
  const init: RequestInit = { method, headers };
  if (body !== undefined) init.body = JSON.stringify(body);
  const exit = yield* Effect.exit(handleCliRequest(new Request(`${origin}/api/cli/${path}`, init)));
  if (Exit.isSuccess(exit)) {
    // SAFETY: test-only view of the handler's JSON; assertions check every field read.
    return { status: 200, json: JSON.parse(JSON.stringify(exit.value)) as Reply, text: JSON.stringify(exit.value) };
  }
  const empty: Reply = {};
  return { status: statusForPublicError(encodePublicError(Cause.squash(exit.cause))), json: empty, text: "" };
});

/** Signs a user up from the CLI, so its session counts as a signed-in device. */
const signUp = Effect.fn(function* (name: string) {
  const auth = yield* Auth;
  const response = yield* auth.handler(new Request(`${origin}/api/auth/sign-up/email`, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "ployz-cli/0.0.0" },
    body: JSON.stringify({ email: `${name}@example.test`, name, password: "correct-horse-battery-staple" }),
  }));
  assert.strictEqual(response.status, 200);
  const cookie = response.headers.get("set-cookie")?.split(";", 1)[0] ?? assert.fail("no session cookie");
  const orgs = yield* cli("GET", "organizations", { cookie });
  const organization = orgs.json.organizations?.[0] ?? assert.fail("no personal Organization");
  return { cookie, organization };
});

it.live(
  "an Organization Token acts in its own Organization until it expires, is revoked, or its maker leaves",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer({ mode: "self_hosted" });
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");

        assert.strictEqual((yield* cli("GET", "tokens", {})).status, 401);
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: "ployz_unknown" })).status, 401);
        assert.strictEqual(
          (yield* cli("POST", "tokens", alice, { name: " ", expires_in_days: 30 })).status, 422);
        assert.strictEqual(
          (yield* cli("POST", "tokens", alice, { name: "ci", expires_in_days: 366 })).status, 422);

        const made = yield* cli("POST", "tokens", alice, { name: "ci", expires_in_days: 30 });
        const token = made.json.token ?? assert.fail("no token");
        assert.match(token.secret, /^ployz_[\w-]{43}$/);
        assert.strictEqual(token.organization, alice.organization.slug);

        // Disclosed once: listings and storage never carry the secret.
        const listed = yield* cli("GET", "tokens", { bearer: token.secret });
        assert.notInclude(listed.text, token.secret);
        assert.deepStrictEqual(listed.json.tokens?.map((row) => [row.id, row.current, row.expired]), [[token.id, true, false]]);
        assert.strictEqual(listed.json.devices?.length, 1);
        const stored = yield* database.drizzle.select().from(organizationToken);
        assert.notInclude(JSON.stringify(stored), token.secret);

        // Bound to one Organization: it sees only that one, and can't touch another's tokens.
        const orgs = yield* cli("GET", "organizations", { bearer: token.secret });
        assert.deepStrictEqual(orgs.json.organizations?.map((row) => [row.slug, row.current]), [[alice.organization.slug, true]]);
        assert.strictEqual((yield* cli("DELETE", `tokens/${token.id}`, bob)).status, 404);
        assert.strictEqual((yield* cli("DELETE", "tokens/not-a-uuid", alice)).status, 404);

        // Expired.
        yield* database.drizzle.update(organizationToken)
          .set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(organizationToken.id, token.id));
        assert.strictEqual((yield* cli("GET", "billing", { bearer: token.secret })).status, 401);
        assert.isTrue((yield* cli("GET", "tokens", alice)).json.tokens?.[0]?.expired);

        // Its maker left the Organization.
        const second = (yield* cli("POST", "tokens", alice, { name: "ci-2", expires_in_days: 1 })).json.token
          ?? assert.fail("no token");
        assert.strictEqual((yield* cli("GET", "billing", { bearer: second.secret })).status, 200);
        const [membership] = yield* database.drizzle.delete(member)
          .where(eq(member.organizationId, alice.organization.id)).returning();
        assert.strictEqual((yield* cli("GET", "billing", { bearer: second.secret })).status, 401);
        // A session whose active Organization is no longer the user's is refused too.
        assert.strictEqual((yield* cli("GET", "billing", alice)).status, 403);
        yield* database.drizzle.insert(member).values(membership ?? assert.fail("no membership"));

        // Revoked.
        const removed = yield* cli("DELETE", `tokens/${second.id}`, alice);
        assert.deepStrictEqual(removed.json.removed, { id: second.id, kind: "token" });
        assert.strictEqual((yield* cli("GET", "billing", { bearer: second.secret })).status, 401);
        assert.strictEqual((yield* cli("DELETE", `tokens/${second.id}`, alice)).status, 404);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "token rm signs out one of the caller's own devices",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer({ mode: "self_hosted" });
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const device = (yield* cli("GET", "tokens", alice)).json.devices?.[0] ?? assert.fail("no device");
        assert.isTrue(device.current);
        assert.strictEqual((yield* cli("DELETE", `tokens/${device.id}`, bob)).status, 404);
        const removed = yield* cli("DELETE", `tokens/${device.id}`, alice);
        assert.deepStrictEqual(removed.json.removed, { id: device.id, kind: "device" });
        assert.lengthOf(yield* database.drizzle.select().from(session).where(eq(session.id, device.id)), 0);
        assert.strictEqual((yield* cli("GET", "tokens", alice)).status, 401);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a Self-hosted Cloud grants custom domains and has no Billing Plan",
  () =>
    Effect.gen(function* () {
      const selfHosted = yield* cliLayer({ mode: "self_hosted" });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const billing = yield* cli("GET", "billing", alice);
        assert.deepInclude(billing.json.billing, { self_hosted: true, pro: false, custom_domains: true });
        assert.strictEqual((yield* cli("POST", "billing/checkout", alice)).status, 404);
      }).pipe(Effect.provide(selfHosted));
    }),
  60_000,
);

it.live(
  "hosted billing reports the plan and the Custom Domain Capability from the cached subscription",
  () =>
    Effect.gen(function* () {
      const hosted = yield* cliLayer(hostedPolar);
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("carol");
        const free = yield* cli("GET", "billing", alice);
        assert.deepInclude(free.json.billing, { self_hosted: false, pro: false, custom_domains: false });
        assert.strictEqual((yield* cli("POST", "billing/checkout", alice)).json.url, "https://polar.test/checkout");

        yield* database.drizzle.insert(organizationBillingState).values({
          organizationId: alice.organization.id,
          hasActiveSubscription: true,
          activeSubscriptionId: "sub",
          currentPeriodEnd: new Date(Date.now() + 86_400_000),
          syncedAt: new Date(),
        });
        const pro = yield* cli("GET", "billing", alice);
        assert.deepInclude(pro.json.billing, { self_hosted: false, pro: true, custom_domains: true });
        assert.strictEqual((yield* cli("POST", "billing/checkout", alice)).status, 409);
        assert.strictEqual((yield* cli("POST", "billing/portal", alice)).json.url, "https://polar.test/portal");
      }).pipe(Effect.provide(hosted));
    }),
  60_000,
);

it.live(
  "stays dark in production",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer({ mode: "self_hosted" }, "production");
      yield* Effect.gen(function* () {
        assert.strictEqual((yield* cli("GET", "organizations", {})).status, 404);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "org use moves a session between its own Organizations only",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer({ mode: "self_hosted" });
      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        // What `ployz org use` sends: better-auth's own endpoint, carried by the session.
        const use = (slug: string) => auth.handler(new Request(`${origin}/api/auth/organization/set-active`, {
          method: "POST",
          headers: { cookie: alice.cookie, origin, "content-type": "application/json" },
          body: JSON.stringify({ organizationSlug: slug }),
        }));
        assert.strictEqual((yield* use(bob.organization.slug)).status, 403);
        yield* database.drizzle.insert(member).values({
          userId: (yield* auth.getSession(new Headers({ cookie: alice.cookie })))?.user.id ?? assert.fail("no user"),
          organizationId: bob.organization.id,
        });
        assert.strictEqual((yield* use(bob.organization.slug)).status, 200);
        const orgs = yield* cli("GET", "organizations", alice);
        assert.deepStrictEqual(
          orgs.json.organizations?.filter((row) => row.current).map((row) => row.slug),
          [bob.organization.slug],
        );
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "each device and token gets its own holder per Server, on first use and on Servers enrolled later; "
    + "revocation clears only its holders and reports offline Servers until a retry confirms",
  () =>
    Effect.gen(function* () {
      const fake = fakeServers();
      const layer = yield* cliLayer({ mode: "self_hosted" }, "test", fake.layer);
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const first = "00000000000000000000000000001239";
        const later = "00000000000000000000000000001240";
        const alice = yield* signUp("alice");
        const device = (yield* cli("GET", "tokens", alice)).json.devices?.[0]?.id ?? assert.fail("no device");
        const token = (yield* cli("POST", "tokens", alice, { name: "ci", expires_in_days: 30 })).json.token
          ?? assert.fail("no token");
        const ci = { bearer: token.secret };

        // Login needed no Server: nothing is paired yet.
        assert.deepStrictEqual((yield* cli("POST", "server-access", alice)).json, { connections: [], unreachable: [] });
        yield* enroll(alice.organization.id, first, true);
        fake.servers.set(first, { online: true, slots: new Map() });

        const access = (yield* cli("POST", "server-access", alice)).json;
        const deviceKey = access.connections?.[0]?.management ?? assert.fail("no capability");
        assert.deepStrictEqual(access, { connections: [{ machine_id: first, management: deviceKey }], unreachable: [] });
        assert.strictEqual(fake.servers.get(first)?.slots.get(serverAccessLabel(device)), deviceKey);
        // Held, not re-provisioned; stored only encrypted.
        assert.deepStrictEqual((yield* cli("POST", "server-access", alice)).json.connections, access.connections);
        assert.strictEqual(fake.sets(), 1);
        assert.notInclude(JSON.stringify(yield* database.drizzle.select().from(serverAccess)), deviceKey);

        // Isolation: the token holds its own slot and key.
        const tokenKey = (yield* cli("POST", "server-access", ci)).json.connections?.[0]?.management;
        assert.notStrictEqual(tokenKey, deviceKey);
        assert.strictEqual(fake.servers.get(first)?.slots.get(serverAccessLabel(token.id)), tokenKey);

        // A Server enrolled after login: unreachable while offline, provisioned once it answers.
        yield* enroll(alice.organization.id, later, false);
        const laterServer = { online: false, slots: new Map<string, string>() };
        fake.servers.set(later, laterServer);
        assert.deepStrictEqual((yield* cli("POST", "server-access", alice)).json.unreachable, [later]);
        laterServer.online = true;
        const both = (yield* cli("POST", "server-access", alice)).json;
        assert.deepStrictEqual(both.connections?.map((row) => row.machine_id).sort(), [first, later]);
        assert.deepStrictEqual(both.unreachable, []);

        // A token can't log out; revoking it clears only its own holder.
        assert.strictEqual((yield* cli("POST", "logout", ci)).status, 422);
        const second = (yield* cli("POST", "tokens", alice, { name: "ci-2", expires_in_days: 30 })).json.token
          ?? assert.fail("no token");
        const ci2 = { bearer: second.secret };
        const third = (yield* cli("POST", "tokens", alice, { name: "ci-3", expires_in_days: 30 })).json.token
          ?? assert.fail("no token");
        const revoked = (yield* cli("DELETE", `tokens/${token.id}`, alice)).json;
        assert.deepStrictEqual(revoked.servers, { confirmed: [first], unconfirmed: [] });
        assert.isFalse(fake.servers.get(first)?.slots.has(serverAccessLabel(token.id)));
        assert.strictEqual(fake.servers.get(first)?.slots.get(serverAccessLabel(device)), deviceKey);

        // Logout with a Server offline: Cloud cuts the device off at once and reports the Server still to confirm.
        laterServer.online = false;
        const out = (yield* cli("POST", "logout", alice)).json;
        assert.deepStrictEqual(out, { signed_out: { id: device }, servers: { confirmed: [first], unconfirmed: [later] } });
        assert.strictEqual((yield* cli("POST", "server-access", alice)).status, 401);
        assert.isTrue(laterServer.slots.has(serverAccessLabel(device)));
        const [pending] = yield* database.drizzle.select().from(serverAccess).where(eq(serverAccess.credentialId, device));
        assert.strictEqual(pending?.encryptedCapability, null);
        assert.deepStrictEqual((yield* cli("GET", "tokens", ci2)).json.revoking,
          [{ id: device, kind: "device", unconfirmed: [later] }]);

        // Rerunning `token rm` retries once the Server is back.
        laterServer.online = true;
        const retried = (yield* cli("DELETE", `tokens/${device}`, ci2)).json;
        assert.deepStrictEqual(retried.removed, { id: device, kind: "device" });
        assert.deepStrictEqual(retried.servers, { confirmed: [later], unconfirmed: [] });
        assert.isFalse(laterServer.slots.has(serverAccessLabel(device)));
        assert.deepStrictEqual((yield* cli("GET", "tokens", ci2)).json.revoking, []);
        assert.strictEqual((yield* cli("DELETE", `tokens/${device}`, ci2)).status, 404);

        // Expiry is caught by the sweep.
        yield* cli("POST", "server-access", ci2);
        assert.isTrue(fake.servers.get(first)?.slots.has(serverAccessLabel(second.id)));
        yield* database.drizzle.update(organizationToken)
          .set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(organizationToken.id, second.id));
        const swept = yield* retireServerAccess();
        assert.deepStrictEqual([...swept.confirmed].sort(), [first, later]);
        assert.isFalse(fake.servers.get(first)?.slots.has(serverAccessLabel(second.id)));
        assert.lengthOf(yield* database.drizzle.select().from(serverAccess), 0);

        // So is a member leaving the Organization.
        yield* cli("POST", "server-access", { bearer: third.secret });
        assert.isTrue(laterServer.slots.has(serverAccessLabel(third.id)));
        yield* database.drizzle.delete(member).where(eq(member.organizationId, alice.organization.id));
        yield* retireServerAccess();
        assert.isFalse(laterServer.slots.has(serverAccessLabel(third.id)));
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "github lists the caller's installations and a readable repository's branches, and disconnects one",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer({ mode: "self_hosted" });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const empty = yield* cli("GET", "github", alice);
        assert.deepInclude(empty.json, { linked: false, ready: false, installations: [], repositories: [] });
        assert.match(empty.json.install_url ?? "", /^https:\/\/github\.com\/apps\/.+\/installations\/new$/);

        const database = yield* Database;
        const [row] = yield* database.drizzle.select({ userId: member.userId }).from(member)
          .where(eq(member.organizationId, alice.organization.id));
        const userId = row?.userId ?? assert.fail("no member");
        yield* database.drizzle.insert(githubInstallation).values({ userId, installationId: 7, accountLogin: "acme", accountType: "Organization" });
        yield* database.drizzle.insert(githubRepositoryCache).values({
          userId, installationId: 7, repositoryId: 11, name: "web", fullName: "acme/web", defaultBranch: "main",
          private: true, htmlUrl: "https://github.com/acme/web", repoUpdatedAt: new Date(),
        });
        const connected = yield* cli("GET", "github", alice);
        assert.deepInclude(connected.json, { ready: true });
        assert.deepStrictEqual(connected.json.installations?.map(({ id, account, repositories }) => ({ id, account, repositories })),
          [{ id: 7, account: "acme", repositories: 1 }]);

        const branches = yield* cli("GET", "github/branches?repository=ACME/web", alice);
        assert.deepInclude(branches.json, { access: "installation", branches: ["dev", "main"] });
        assert.strictEqual((yield* cli("GET", "github/branches?repository=acme/secret", alice)).status, 404);

        const removed = yield* cli("DELETE", "github/7", alice);
        assert.deepStrictEqual(removed.json.disconnected, { id: 7, account: "acme" });
        assert.strictEqual(removed.json.uninstall_url, "https://github.com/organizations/acme/settings/installations/7");
        assert.strictEqual((yield* cli("DELETE", "github/7", alice)).status, 404);
        assert.deepInclude((yield* cli("GET", "github", alice)).json, { ready: false, installations: [] });
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
