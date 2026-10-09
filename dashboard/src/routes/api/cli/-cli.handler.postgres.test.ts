import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { vi } from "vitest";
import { createHash } from "node:crypto";
import type { Client, DockerVolumeName, MachineId, RuntimeWatchView } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Cause, ConfigProvider, Effect, Exit, Layer } from "effect";
import { Inngest } from "inngest";
import { Polar } from "#/modules/billing/polar-provider.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { githubInstallation, githubRepositoryCache } from "#/modules/github/tables";
import { InngestClient } from "#/modules/inngest/client";
import { member, organizationToken, session } from "#/modules/identity/tables";
import { asTestDouble } from "#/lib/test-double";
import type { JsonObject } from "#/db/tables";
import { retireServerAccess, serverAccessLabel } from "#/modules/machines/server-access.server";
import { machineRemoveAttempt, organizationMachine, serverAccess } from "#/modules/machines/tables";
import { makePloyzLayer, Ployz } from "#/modules/runtime/ployz.server";
import { OrganizationRuntime, OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { DEFAULT_SERVER_UPGRADE_SETTINGS } from "#/modules/server-upgrade/server-upgrade";
import { operationApprovals } from "#/modules/approvals/tables";
import { callStore } from "#/modules/config-store/config-store.server";
import { CloudStoreLive } from "#/modules/config-store/store-sdk.server";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { volumeRun } from "#/modules/volume-run/tables";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { handleCliRequest as handleOrganizationCliRequest } from "#/routes/api/cli/-handlers";
import { MintMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { mintCliMachineEnrollment } from "#/modules/machines/enrollment.server";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import { fakeGithubApi } from "#/test/fake-github";
import { postgresTestDatabase } from "#/test/postgres";

const origin = "http://localhost:3000";

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
        inspect: async () => ({ management_clients: [...server.slots.keys()] }),
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

const cliLayer = Effect.fn(function* (
  ployz: Layer.Layer<Ployz> = fakeServers().layer,
  overrides: { readonly runtime?: Layer.Layer<OrganizationRuntime>; readonly inngest?: Inngest } = {},
) {
  const testDatabase = yield* postgresTestDatabase;
  const provider = ConfigProvider.fromEnv({
    env: { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: testDatabase.url.href },
  });
  const configLayer = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(provider)));
  const databaseLayer = DatabaseLive.pipe(Layer.provide(configLayer));
  const services = Layer.mergeAll(
    configLayer,
    databaseLayer,
    Layer.succeed(Polar, { mode: "self_hosted" }),
    Layer.succeed(InngestClient, overrides.inngest ?? new Inngest({ id: "cli-test" })),
    Layer.succeed(SecretEncryption, encryption),
    Layer.succeed(GithubApi, github.service),
    ployz,
  );
  const store = CloudStoreLive.pipe(Layer.provide(Layer.merge(configLayer, databaseLayer)));
  const runtime = overrides.runtime ?? OrganizationRuntimeLive.pipe(Layer.provide(services));
  return Layer.mergeAll(AuthLive.pipe(Layer.provide(services)), runtime, services, store);
});

/** The fields these tests read from `/api/cli` replies. */
type Reply = {
  readonly run?: { readonly id: string; readonly state: string };
  readonly organizations?: ReadonlyArray<{ readonly id: string; readonly slug: string; readonly current: boolean }>;
  readonly token?: { readonly id: string; readonly secret: string; readonly organization: string };
  readonly tokens?: ReadonlyArray<{ readonly id: string; readonly current: boolean; readonly expired: boolean }>;
  readonly devices?: ReadonlyArray<{ readonly id: string; readonly current: boolean }>;
  readonly removed?: boolean | { readonly id: string; readonly kind: string };
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
  readonly organization?: string;
  readonly namespace?: string;
  readonly volumes?: ReadonlyArray<unknown>;
  readonly approval?: { readonly id: string; readonly status: string; readonly digest: string };
  readonly error?: {
    readonly code: string;
    readonly message: string;
    readonly details: {
      readonly next?: string;
      readonly approval?: string;
      readonly approval_id?: string;
      readonly channel?: string;
      readonly effects?: ReadonlyArray<{ readonly kind: string; readonly name: string }>;
      readonly operation?: { readonly verb: string; readonly name: string };
    };
  };
  readonly id?: string;
  readonly state?: string;
  readonly revoking?: ReadonlyArray<{ readonly id: string; readonly kind: string; readonly unconfirmed: ReadonlyArray<string> }>;
};

type As = { readonly cookie?: string; readonly bearer?: string; readonly approval?: string };

type CliBody = JsonObject;

const cli = Effect.fn(function* (method: string, path: string, as: As, body?: CliBody) {
  const headers = new Headers();
  if (as.cookie !== undefined) headers.set("cookie", as.cookie);
  if (as.bearer !== undefined) headers.set("authorization", `Bearer ${as.bearer}`);
  if (as.approval !== undefined) headers.set("x-ployz-approval", as.approval);
  const init: RequestInit = { method, headers };
  if (body !== undefined) init.body = JSON.stringify(body);
  const exit = yield* Effect.exit(handleCliRequest(new Request(`${origin}/api/cli/${path}`, init)));
  if (Exit.isSuccess(exit) && exit.value instanceof Response) {
    const text = yield* Effect.promise(() => (exit.value as Response).text());
    // SAFETY: test-only view of the handler's JSON; assertions check every field read.
    return { status: exit.value.status, json: JSON.parse(text) as Reply, text };
  }
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
      const layer = yield* cliLayer();
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
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: token.secret })).status, 401);
        assert.isTrue((yield* cli("GET", "tokens", alice)).json.tokens?.[0]?.expired);

        // Its maker left the Organization.
        const second = (yield* cli("POST", "tokens", alice, { name: "ci-2", expires_in_days: 1 })).json.token
          ?? assert.fail("no token");
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: second.secret })).status, 200);
        const [membership] = yield* database.drizzle.delete(member)
          .where(eq(member.organizationId, alice.organization.id)).returning();
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: second.secret })).status, 401);
        // A session whose active Organization is no longer the user's is refused too.
        assert.strictEqual((yield* cli("GET", "tokens", alice)).status, 403);
        yield* database.drizzle.insert(member).values(membership ?? assert.fail("no membership"));

        // Revoked.
        const removed = yield* cli("DELETE", `tokens/${second.id}`, alice);
        assert.deepStrictEqual(removed.json.removed, { id: second.id, kind: "token" });
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: second.secret })).status, 401);
        assert.strictEqual((yield* cli("DELETE", `tokens/${second.id}`, alice)).status, 404);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "token rm signs out one of the caller's own devices",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
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
  "a volume run the caller cannot see answers as a not_found refusal, not a bare 404",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const [run] = yield* database.drizzle.insert(volumeRun).values({
          organizationId: bob.organization.id,
          environmentId: "env-1",
          volumeId: "vol-1",
          volumeName: "data",
          dockerVolume: "ns_vol-1",
          kind: "sync",
          args: { full: false },
        }).returning();
        const id = run?.id ?? assert.fail("no run row");
        assert.deepInclude((yield* cli("GET", `volume-runs/${id}`, bob)).json.run, { id, state: "requested" });
        for (const path of [`volume-runs/${id}`, "volume-runs/00000000-0000-4000-8000-000000000000", "volume-runs/not-a-run"]) {
          const missing = yield* cli("GET", path, alice);
          assert.strictEqual(missing.status, 404, path);
          assert.strictEqual(missing.json.error?.code, "not_found", path);
        }
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** The bodies `ployz volume mirror|sync|mirror rm|move|release` send, pinned the same in the CLI's `each_kind_posts_only_its_own_fields`. */
const cliVolumeRunBodies: ReadonlyArray<CliBody> = [
  { environment: { project: "shop", environment: "production" }, kind: "mirror", to: "web-2" },
  { environment: { project: "shop", environment: "production" }, kind: "sync", full: false },
  { environment: { project: "shop", environment: "production" }, kind: "delete_mirror", slot: "web-2", confirm: "data" },
  { environment: { project: "shop", environment: "production" }, kind: "move", to: "web-2" },
  { environment: { project: "shop", environment: "production" }, kind: "release" },
];

it.live(
  "Cloud decodes every volume run body the CLI sends, and refuses an Environment named by id",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        for (const body of cliVolumeRunBodies) {
          const reply = yield* cli("POST", "volumes/vol-1/runs", alice, body);
          assert.notStrictEqual(reply.status, 422, JSON.stringify(body));
        }
        const byId = yield* cli("POST", "volumes/vol-1/runs", alice, { environment: "env-1", kind: "sync" });
        assert.strictEqual(byId.status, 422);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "org use moves a session between its own Organizations only",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
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
      const layer = yield* cliLayer(fake.layer);
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
      const layer = yield* cliLayer();
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

it.live(
  "org rm refuses while the Organization has a Project, then unpairs every Server, clearing device keys, "
    + "and keeps the Organization disabled until an offline Server confirms",
  () =>
    Effect.gen(function* () {
      const fake = fakeServers();
      const layer = yield* cliLayer(fake.layer);
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const machine = "00000000000000000000000000001260";
        const alice = yield* signUp("alice");
        const slug = alice.organization.slug;
        const [owner] = yield* database.drizzle.select({ userId: member.userId }).from(member)
          .where(eq(member.organizationId, alice.organization.id));
        const userId = owner?.userId ?? assert.fail("no member");
        yield* enroll(alice.organization.id, machine, true);
        const server = { online: true, slots: new Map<string, string>([["cloud", "cloud-key"]]) };
        fake.servers.set(machine, server);
        const device = (yield* cli("GET", "tokens", alice)).json.devices?.[0]?.id ?? assert.fail("no device");
        yield* cli("POST", "server-access", alice);
        assert.isTrue(server.slots.has(serverAccessLabel(device)));

        // Only the Organization the credential acts in.
        const other = yield* cli("DELETE", "organizations/elsewhere", alice);
        assert.strictEqual(other.status, 422);
        assert.strictEqual(other.json.error?.details.next, "ployz org use elsewhere");

        // A Project must go first, through its own teardown; nothing is unpaired meanwhile.
        const write = (command: Parameters<typeof callStore>[2]) => callStore(alice.organization.id, userId, command);
        const created = yield* write({ operation: "write", command: {
          command: "create_project", id: "00000000-0000-4000-8000-000000001260", name: "shop",
          default_environment: "00000000-0000-4000-8000-000000001261",
        } });
        assert.isTrue(created.ok);
        const refused = yield* cli("DELETE", `organizations/${slug}`, alice);
        assert.strictEqual(refused.status, 409);
        assert.strictEqual(refused.json.error?.details.next, "ployz project rm shop --confirm shop");
        assert.isTrue(server.slots.has("cloud"));
        assert.isTrue((yield* write({ operation: "write", command: { command: "remove_project", project: "shop" } })).ok);

        // A Server offline: the Organization stays, disabled, and says which Server must still confirm.
        server.online = false;
        const partial = yield* cli("DELETE", `organizations/${slug}`, alice);
        assert.deepInclude(partial.json, { organization: slug, removed: false, servers: { confirmed: [], unconfirmed: [machine] } });
        assert.lengthOf(yield* database.drizzle.select().from(organization).where(eq(organization.id, alice.organization.id)), 1);
        assert.lengthOf(yield* database.drizzle.select().from(organizationPairing), 1);

        // The same command confirms it once the Server is back, clearing Cloud's key and every device key.
        server.online = true;
        const removed = yield* cli("DELETE", `organizations/${slug}`, alice);
        assert.deepInclude(removed.json, { organization: slug, removed: true, servers: { confirmed: [machine], unconfirmed: [] } });
        assert.deepStrictEqual([...server.slots.keys()], []);
        assert.lengthOf(yield* database.drizzle.select().from(organization).where(eq(organization.id, alice.organization.id)), 0);
        assert.lengthOf(yield* database.drizzle.select().from(organizationPairing), 0);
        assert.lengthOf(yield* database.drizzle.select().from(serverAccess), 0);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "enrollment takes a signed-in device or an Organization Token, the token only in its own Organization",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const token = (yield* cli("POST", "tokens", alice, { name: "ci", expires_in_days: 1 })).json.token ?? assert.fail("no token");
        const enroll = (as: As, body: Readonly<Record<string, string | boolean>>) => {
          const headers = new Headers();
          if (as.cookie !== undefined) headers.set("cookie", as.cookie);
          if (as.bearer !== undefined) headers.set("authorization", `Bearer ${as.bearer}`);
          const request = new Request(`${origin}/api/cli/servers/enroll`, { method: "POST", headers, body: JSON.stringify(body) });
          return handleOrganizationCliRequest(request, MintMachineEnrollmentInput, mintCliMachineEnrollment).pipe(
            Effect.map((response) => response.status),
            Effect.catch((error) => Effect.succeed(statusForPublicError(encodePublicError(error)))),
          );
        };
        assert.strictEqual(yield* enroll(alice, { organizationSlug: alice.organization.slug }), 200);
        assert.strictEqual(yield* enroll({ bearer: token.secret }, { organizationSlug: alice.organization.slug }), 200);
        assert.strictEqual(yield* enroll({ bearer: token.secret }, { organizationSlug: bob.organization.slug }), 403);
        assert.strictEqual(yield* enroll({}, { organizationSlug: alice.organization.slug }), 401);
        assert.strictEqual(yield* enroll(alice, { organizationSlug: alice.organization.slug, extra: true }), 422);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "the CLI reads and decides only its own Organization's approvals, and only for the plan they ask about",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        // Asked about Environment `production` of a Project the Store has no more: its plan moved on.
        const [row] = yield* drizzle.insert(operationApprovals).values({
          organizationId: alice.organization.id, subject: "00000000-0000-4000-8000-000000001301",
          credentialKind: "session", credentialId: "device", command: "publish", digest: "7:abc",
          review: asTestDouble<typeof operationApprovals.$inferInsert.review>()({
            effects: [], diff: { version: "7", environment: { id: "00000000-0000-4000-8000-000000001301", project: "gone", name: "production" } },
          }),
        }).returning({ id: operationApprovals.id });
        const id = row?.id ?? assert.fail("no approval");

        assert.strictEqual((yield* cli("GET", `approvals/${id}`, bob)).status, 404);
        assert.strictEqual((yield* cli("POST", `approvals/${id}`, bob, { reject: {} })).status, 404);
        assert.strictEqual((yield* cli("GET", "approvals/not-a-uuid", alice)).status, 404);
        assert.strictEqual((yield* cli("POST", `approvals/${id}`, alice, { approve: "yes" })).status, 422);

        const read = yield* cli("GET", `approvals/${id}`, alice);
        assert.strictEqual(read.status, 200);
        assert.deepInclude(read.json.approval, { id, status: "superseded", digest: "7:abc" });
        const approved = yield* cli("POST", `approvals/${id}`, alice, { approve: { digest: "7:abc" } });
        assert.strictEqual(approved.status, 409);
        assert.strictEqual(approved.json.error?.code, "conflict");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

const fra1 = "f".repeat(32);
const leftBehind = { kind: "docker_volume" as const, id: { machine_id: fra1 as MachineId, name: "left-behind_data" as DockerVolumeName } };

type LeftBehind = { observesFra1: boolean; sessionsOpened: number };

const leftBehindClusterOf = (cluster: LeftBehind) => Layer.succeed(OrganizationRuntime, {
  cancel: () => Effect.void,
  open: () => Effect.sync(() => ({
    status: "connected" as const,
    connected: asTestDouble<PloyzSession>()({
      watchFirstFrame: () => Effect.succeed(asTestDouble<RuntimeWatchView>()({
        machines: cluster.observesFra1 ? [{ machine: { id: fra1, name: "fra-1" } }] : [],
        services: [{
          identity: "left-behind/web",
          service_id: "web",
          containers: [{
            kind: "service_container", machine_id: fra1, runtime: { state: "running" }, created_at_unix_nanos: 1,
            resolved_spec: {
              mode: { mode: "replicated" },
              volumes: [{ reference: "data", source: { kind: "ordinary", name: "left-behind_data" } }],
              mounts: [{ volume: "data", target: "/data" }],
            },
          }],
        }],
      })),
      dataLossIfNamespaceDestroyed: () => Effect.succeed({ data_loss: [leftBehind] }),
    }),
  })).pipe(Effect.tap(() => Effect.sync(() => { cluster.sessionsOpened += 1; }))),
});

const leftBehindCluster = leftBehindClusterOf({ observesFra1: true, sessionsOpened: 0 });

it.live(
  "the CLI's clean, drain and remove ask a human before they destroy anything, and an approved retry starts one run",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-operations-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindCluster, inngest });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");

        const asked = yield* cli("POST", "namespaces/left-behind/clean", alice);
        assert.strictEqual(asked.status, 409);
        assert.strictEqual(asked.json.error?.code, "approval_required");
        const details = asked.json.error?.details ?? assert.fail("no details");
        assert.deepStrictEqual(details.operation, { verb: "clean", name: "left-behind" });
        assert.deepStrictEqual(details.effects?.map(({ kind, name }) => [kind, name]), [
          ["removes_service", "left-behind/web"],
          ["deletes_volume", "used by web at /data on fra-1"],
        ]);
        const approvalId = details.approval_id ?? assert.fail("no approval id");
        assert.lengthOf(sent.mock.calls, 0);

        const pending = yield* cli("POST", "namespaces/left-behind/clean", { ...alice, approval: approvalId });
        assert.strictEqual(pending.status, 409);
        assert.strictEqual(pending.json.error?.details.approval_id, approvalId);

        const approved = yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: details.approval ?? "" } });
        assert.strictEqual(approved.status, 200);
        assert.strictEqual(approved.json.approval?.status, "approved");

        const started = yield* cli("POST", "namespaces/left-behind/clean", { ...alice, approval: approvalId });
        assert.strictEqual(started.status, 202);
        const cleanupId = started.json.id ?? assert.fail("no clean id");
        assert.deepStrictEqual((yield* cli("GET", `namespace-cleanups/${cleanupId}`, alice)).json, { state: "pending" });
        const again = yield* cli("POST", "namespaces/left-behind/clean", { ...alice, approval: approvalId });
        assert.strictEqual(again.json.id, cleanupId);
        assert.deepStrictEqual(sent.mock.calls.map(([event]) => (event as { id?: string }).id), [
          `namespace-cleanup-${cleanupId}`, `namespace-cleanup-${cleanupId}`,
        ]);
        assert.strictEqual((yield* cli("GET", "namespace-cleanups/not-a-uuid", alice)).json.error?.code, "not_found");

        const removal = yield* cli("DELETE", `servers/${fra1}`, alice, { no_reset: true });
        assert.strictEqual(removal.status, 409);
        assert.deepStrictEqual(removal.json.error?.details.operation, { verb: "remove", name: "fra-1" });

        const drain = yield* cli("POST", `servers/${fra1}/drain`, alice);
        assert.strictEqual(drain.status, 202);
        const drainId = drain.json.id ?? assert.fail("no drain id");
        assert.deepStrictEqual((yield* cli("GET", `server-drains/${drainId}`, alice)).json, { state: "pending" });
        assert.strictEqual((yield* cli("GET", "server-drains/not-a-uuid", alice)).json.error?.code, "not_found");
        const notAMachine = yield* cli("POST", "servers/not-a-machine/drain", alice);
        assert.deepStrictEqual([notAMachine.status, notAMachine.json.error?.message], [404, "No such Server."]);
        const unseen = yield* cli("POST", `servers/${"e".repeat(32)}/drain`, alice);
        assert.deepStrictEqual([unseen.status, unseen.json.error?.message], [404, "No such Server."]);

        const approvals = yield* drizzle.select({ subject: operationApprovals.subject, status: operationApprovals.status })
          .from(operationApprovals).orderBy(operationApprovals.subject);
        assert.deepStrictEqual(approvals.map(({ subject, status }) => [subject, status]), [
          ["namespace:left-behind", "approved"],
          [`server:${fra1}`, "pending"],
        ]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "an approved `server rm` removes once, and a removal that would reset the Server isn't covered by a keep-data approval",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-remove-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindCluster, inngest });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        const keep = { no_reset: true };
        const asked = (yield* cli("DELETE", `servers/${fra1}`, alice, keep)).json.error?.details ?? assert.fail("no ask");
        const approvalId = asked.approval_id ?? assert.fail("no approval id");
        yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: asked.approval ?? "" } });

        const reset = yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, { confirm_data_loss: { confirmed: [] } });
        assert.strictEqual(reset.json.error?.code, "approval_required");
        assert.notStrictEqual(reset.json.error?.details.approval_id, approvalId);

        const started = yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, keep);
        const removalId = started.json.id ?? assert.fail("no removal id");
        const resetAfterUse = yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, { confirm_data_loss: { confirmed: [] } });
        assert.strictEqual(resetAfterUse.json.error?.code, "approval_required");
        const elsewhere = yield* cli("DELETE", `servers/${"e".repeat(32)}`, { ...alice, approval: approvalId }, keep);
        assert.strictEqual(elsewhere.json.error?.code, "not_found");
        const ended = new Date();
        yield* drizzle.update(machineRemoveAttempt).set({
          state: "failed", inngestRunId: "run-1", startedAt: ended, terminalAt: ended, failureCode: "unreachable", failureMessage: "gone",
        }).where(eq(machineRemoveAttempt.id, removalId));
        assert.strictEqual((yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, keep)).json.id, removalId);
        assert.deepStrictEqual(sent.mock.calls.map(([event]) => (event as { id?: string }).id), [`machine-remove-${removalId}`]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a used `server rm` approval answers with its removal once the Server is gone, and a Server Cloud no longer sees is never asked about",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-remove-gone-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const cluster: LeftBehind = { observesFra1: true, sessionsOpened: 0 };
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindClusterOf(cluster), inngest });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        const keep = { no_reset: true };
        const asked = (yield* cli("DELETE", `servers/${fra1}`, alice, keep)).json.error?.details ?? assert.fail("no ask");
        const approvalId = asked.approval_id ?? assert.fail("no approval id");
        yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: asked.approval ?? "" } });
        const removalId = (yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, keep)).json.id
          ?? assert.fail("no removal id");
        const ended = new Date();
        yield* drizzle.update(machineRemoveAttempt).set({ state: "succeeded", inngestRunId: "run-1", startedAt: ended, terminalAt: ended })
          .where(eq(machineRemoveAttempt.id, removalId));
        cluster.observesFra1 = false;

        const retried = yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, keep);
        assert.strictEqual(retried.status, 200);
        assert.strictEqual(retried.json.id, removalId);
        for (const body of [keep, { confirm_data_loss: { confirmed: [] } }]) {
          const gone = yield* cli("DELETE", `servers/${fra1}`, alice, body);
          assert.strictEqual(gone.status, 404);
          assert.deepStrictEqual([gone.json.error?.code, gone.json.error?.message], ["not_found", "No such Server."]);
        }

        const approvals = yield* drizzle.select({ status: operationApprovals.status }).from(operationApprovals);
        assert.deepStrictEqual(approvals.map(({ status }) => status), ["approved"]);
        assert.deepStrictEqual(sent.mock.calls.map(([event]) => (event as { id?: string }).id), [`machine-remove-${removalId}`]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a clean names the Volumes it would delete before anyone confirms, and plans on one Engine session",
  () =>
    Effect.gen(function* () {
      const cluster: LeftBehind = { observesFra1: true, sessionsOpened: 0 };
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindClusterOf(cluster) });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const preview = yield* cli("GET", "namespaces/left-behind/clean", alice);
        assert.strictEqual(preview.status, 200);
        assert.deepStrictEqual(preview.json, { namespace: "left-behind", volumes: [leftBehind.id] });
        assert.strictEqual(cluster.sessionsOpened, 1);

        cluster.sessionsOpened = 0;
        assert.strictEqual((yield* cli("POST", "namespaces/left-behind/clean", alice)).json.error?.code, "approval_required");
        assert.strictEqual(cluster.sessionsOpened, 1);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "the CLI's Upgrade refuses a Release Channel other than the Organization's, naming the one it follows",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindCluster });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const refused = yield* cli("POST", `servers/${fra1}/upgrade`, alice, { channel: "no-such-channel" });
        assert.strictEqual(refused.status, 409);
        assert.strictEqual(refused.json.error?.code, "channel_mismatch");
        assert.deepStrictEqual(refused.json.error?.details, { channel: DEFAULT_SERVER_UPGRADE_SETTINGS.channel });
        assert.strictEqual((yield* cli("GET", "server-upgrades/not-an-attempt", alice)).json.error?.code, "not_found");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
