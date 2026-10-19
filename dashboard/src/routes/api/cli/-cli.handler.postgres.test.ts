import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { vi } from "vitest";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import type { Client, DockerVolumeName, MachineId, RuntimeWatchView } from "@ployz/sdk";
import { and, eq, sql } from "drizzle-orm";
import { Cause, ConfigProvider, Effect, Exit, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { Polar } from "#/modules/billing/polar-provider.server";
import { GithubRepositoriesLive } from "#/modules/github/github-cli.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { githubInstallation, githubRepositoryCache, organizationGithubInstallation } from "#/modules/github/tables";
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
import { decideApproval } from "#/modules/approvals/approvals.server";
import { operationApprovals } from "#/modules/approvals/tables";
import type { Caller } from "#/modules/identity/actor";
import { forgetServers } from "#/modules/machines/forget-servers.server";
import { forgetServer } from "#/modules/operations/operations.server";
import { removeOrganizationAsMember } from "#/modules/organization/organization-removal.server";
import { callStore, readStore } from "#/modules/config-store/config-store.server";
import { permissionsOf } from "#/modules/organization/permissions";
import { CloudStoreLive } from "#/modules/config-store/store-sdk.server";
import { organization } from "#/modules/organization/tables";
import { syncOrganizationSlug } from "#/modules/organization/organization-state.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { fleetToken, serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { volumeRun } from "#/modules/volume-run/tables";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { handleCliRequest as handleOrganizationCliRequest } from "#/routes/api/cli/-handlers";
import { CliMintMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { mintCliMachineEnrollment } from "#/modules/machines/enrollment.server";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { encodePublicError, statusForPublicError, Unauthorized } from "#/server/public-error";
import { createOrganizationForSession } from "#/modules/organization/organization-state.server";
import { fakeGithubApi } from "#/test/fake-github";
import * as ids from "#/test/ids";
import { postgresTestDatabase } from "#/test/postgres";

const origin = "http://localhost:3000";

/** GitHub: the private acme/web (through installation 7) has branches main and dev, a Dockerfile and a src directory. */
const github = fakeGithubApi({
  "/repos/acme/web/branches?per_page=100&page=1": [{ name: "main" }, { name: "dev" }],
  "/repos/acme/web/commits/main": { commit: { tree: { sha: "a".repeat(40) } } },
  [`/repos/acme/web/git/trees/${"a".repeat(40)}?recursive=1`]: {
    truncated: false,
    tree: [
      { path: "Dockerfile", type: "blob", mode: "100644" },
      { path: "src", type: "tree", mode: "040000" },
      { path: "src/main.ts", type: "blob", mode: "100644" },
    ],
  },
  "/repos/acme/web/contents/Dockerfile?ref=main": {
    type: "file", size: 12, encoding: "base64", content: Buffer.from("FROM alpine\n").toString("base64"),
  },
  "/repos/acme/web/contents/src?ref=main": [{ path: "src/main.ts" }],
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
        setPushTarget: async () => undefined,
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
const enroll = Effect.fn(function* (organizationId: string, machineId: MachineId, first: boolean) {
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
  overrides: {
    readonly runtime?: Layer.Layer<OrganizationRuntime>;
    readonly inngest?: Inngest;
    readonly env?: Record<string, string>;
  } = {},
) {
  const testDatabase = yield* postgresTestDatabase;
  const provider = ConfigProvider.fromEnv({
    env: { ...testConfigEnvironment(), NODE_ENV: "test", DATABASE_URL: testDatabase.url.href, ...overrides.env },
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
  const repositories = GithubRepositoriesLive.pipe(Layer.provide(services));
  return Layer.mergeAll(AuthLive.pipe(Layer.provide(services)), runtime, services, store, repositories);
});

/** The fields these tests read from `/api/cli` replies. */
const Named = Schema.Struct({ id: Schema.String, kind: Schema.String });
const Approval = Schema.Struct({ id: Schema.String, status: Schema.String, digest: Schema.String });
const MadeOrganization = Schema.Struct({ id: Schema.String, slug: Schema.String, name: Schema.String });
const Reply = Schema.Struct({
  run: Schema.optionalKey(Schema.Struct({ id: Schema.String, state: Schema.String })),
  organizations: Schema.optionalKey(Schema.Array(Schema.Struct({ id: Schema.String, slug: Schema.String, current: Schema.Boolean }))),
  token: Schema.optionalKey(Schema.Struct({ id: Schema.String, secret: Schema.String, organization: Schema.String, kind: Schema.String })),
  tokens: Schema.optionalKey(Schema.Array(Schema.Struct({
    id: Schema.String, kind: Schema.String, current: Schema.Boolean, expired: Schema.Boolean,
  }))),
  fleet_token: Schema.optionalKey(Schema.Struct({ id: Schema.String, name: Schema.String, token: Schema.String, expires_at: Schema.String })),
  fleet_tokens: Schema.optionalKey(Schema.Array(Schema.Struct({
    id: Schema.String, name: Schema.String, expired: Schema.Boolean, auto_enroll_max: Schema.NullOr(Schema.Number),
  }))),
  waiting_removed: Schema.optionalKey(Schema.Number),
  devices: Schema.optionalKey(Schema.Array(Schema.Struct({ id: Schema.String, current: Schema.Boolean }))),
  removed: Schema.optionalKey(Schema.Union([Schema.Boolean, Named])),
  connections: Schema.optionalKey(Schema.Array(Schema.Struct({ machine_id: Schema.String, management: Schema.String }))),
  unreachable: Schema.optionalKey(Schema.Array(Schema.String)),
  servers: Schema.optionalKey(Schema.Union([
    Schema.Struct({ confirmed: Schema.Array(Schema.String), unconfirmed: Schema.Array(Schema.String) }),
    Schema.Array(Schema.Struct({ id: Schema.String, name: Schema.String, reach: Schema.String })),
  ])),
  signed_out: Schema.optionalKey(Schema.Struct({ id: Schema.String })),
  ready: Schema.optionalKey(Schema.Boolean),
  connect_url: Schema.optionalKey(Schema.String),
  installations: Schema.optionalKey(Schema.Array(Schema.Struct({
    id: Schema.Number, account: Schema.String, repositories: Schema.Number, connected_by: Schema.NullOr(Schema.String),
  }))),
  repositories: Schema.optionalKey(Schema.Array(Schema.Struct({ repository: Schema.String, installation: Schema.Number }))),
  branches: Schema.optionalKey(Schema.Array(Schema.String)),
  access: Schema.optionalKey(Schema.String),
  disconnected: Schema.optionalKey(Schema.Struct({ id: Schema.Number, account: Schema.String })),
  uninstall_url: Schema.optionalKey(Schema.String),
  ref: Schema.optionalKey(Schema.String),
  paths: Schema.optionalKey(Schema.Array(Schema.String)),
  repository: Schema.optionalKey(Schema.String),
  path: Schema.optionalKey(Schema.String),
  size: Schema.optionalKey(Schema.Number),
  truncated: Schema.optionalKey(Schema.Boolean),
  content: Schema.optionalKey(Schema.NullOr(Schema.String)),
  organization: Schema.optionalKey(Schema.Union([Schema.String, MadeOrganization])),
  namespace: Schema.optionalKey(Schema.String),
  volumes: Schema.optionalKey(Schema.Array(Schema.Unknown)),
  approval: Schema.optionalKey(Approval),
  error: Schema.optionalKey(Schema.Struct({
    code: Schema.String,
    message: Schema.String,
    details: Schema.NullOr(Schema.Struct({
      next: Schema.optionalKey(Schema.Union([
        Schema.String,
        Schema.Struct({ tool: Schema.String, input: Schema.Record(Schema.String, Schema.String) }),
      ])),
      approval: Schema.optionalKey(Schema.Union([Schema.String, Approval])),
      approval_id: Schema.optionalKey(Schema.String),
      channel: Schema.optionalKey(Schema.String),
      effects: Schema.optionalKey(Schema.Array(Schema.Struct({ kind: Schema.String, name: Schema.String }))),
      operation: Schema.optionalKey(Schema.Struct({ verb: Schema.String, name: Schema.String })),
      valid_children: Schema.optionalKey(Schema.Array(Schema.String)),
    })),
  })),
  id: Schema.optionalKey(Schema.String),
  state: Schema.optionalKey(Schema.String),
  revoking: Schema.optionalKey(Schema.Array(Schema.Struct({
    id: Schema.String, kind: Schema.String, unconfirmed: Schema.Array(Schema.String),
  }))),
});
type Reply = typeof Reply.Type;
/** Checks the fields the tests read and keeps the rest, so whole-reply assertions still see every field. */
const decodeReply = (text: string) => Schema.decodeUnknownSync(Reply)(JSON.parse(text), { onExcessProperty: "preserve" });

/** `session` is a signed-in login's session token, which the CLI sends as a bearer token. */
type As = {
  readonly session?: string;
  readonly bearer?: string;
  readonly approval?: string;
  readonly key?: string;
  readonly via?: string;
};

type CliBody = JsonObject;

const cli = Effect.fn(function* (method: string, path: string, as: As, body?: CliBody) {
  const headers = new Headers();
  const bearer = as.session ?? as.bearer;
  if (bearer !== undefined) headers.set("authorization", `Bearer ${bearer}`);
  if (as.approval !== undefined) headers.set("x-ployz-approval", as.approval);
  if (as.key !== undefined) headers.set("x-ployz-request-key", as.key);
  if (as.via !== undefined) headers.set("x-ployz-via", as.via);
  const init: RequestInit = { method, headers };
  if (body !== undefined) init.body = JSON.stringify(body);
  const exit = yield* Effect.exit(handleCliRequest(new Request(`${origin}/api/cli/${path}`, init)));
  if (Exit.isSuccess(exit) && exit.value instanceof Response) {
    const text = yield* Effect.promise(() => (exit.value as Response).text());
    return { status: exit.value.status, json: decodeReply(text), text };
  }
  if (Exit.isSuccess(exit)) {
    const text = JSON.stringify(exit.value);
    return { status: 200, json: decodeReply(text), text };
  }
  const empty: Reply = {};
  const error = encodePublicError(Cause.squash(exit.cause));
  return { status: statusForPublicError(error), json: empty, text: JSON.stringify(error) };
});

/** The Organization `POST organizations` made. */
const madeOrganization = (reply: { status: number; json: Reply }) => {
  assert.strictEqual(reply.status, 200);
  const made = reply.json.organization;
  return Schema.is(MadeOrganization)(made) ? made : assert.fail("no Organization made");
};

/** A refusal's status and message, whether the route answered it or failed with it. */
const refusedWith = (reply: { status: number; text: string }) => {
  const body = JSON.parse(reply.text) as { message?: string; error?: { message?: string } };
  return [reply.status, body.error?.message ?? body.message];
};

/** Signs a user up from the CLI, so its session counts as a signed-in device. */
const signUp = Effect.fn(function* (name: string) {
  const auth = yield* Auth;
  const response = yield* auth.handler(new Request(`${origin}/api/auth/sign-up/email`, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "ployz-cli/0.0.0" },
    body: JSON.stringify({ email: `${name}@example.test`, name, password: "correct-horse-battery-staple" }),
  }));
  assert.strictEqual(response.status, 200);
  const session = response.headers.get("set-auth-token") ?? assert.fail("no session token");
  const cookie = response.headers.get("set-cookie")?.split(";", 1)[0] ?? assert.fail("no session cookie");
  const orgs = yield* cli("GET", "organizations", { session });
  const organization = orgs.json.organizations?.[0] ?? assert.fail("no personal Organization");
  return { session, cookie, organization };
});

/** Approves what `asked` waits on, as `userId` in the dashboard, and returns the approval's ID for the retry. */
const approveAsked = Effect.fn(function* (asked: { json: Reply; text: string }, userId: string, organizationId: string) {
  assert.strictEqual(asked.json.error?.code, "approval_required", asked.text);
  const id = asked.json.error?.details?.approval_id ?? assert.fail("no approval id");
  const digest = Schema.decodeUnknownSync(Schema.String)(asked.json.error?.details?.approval);
  assert.isTrue((yield* decideApproval({ userId, organization: { id: organizationId }, via: "dashboard" }, id, { approve: { digest } })).ok);
  return id;
});

/** Signs `name` back in from the CLI, as a new device. */
const signIn = Effect.fn(function* (name: string) {
  const auth = yield* Auth;
  const response = yield* auth.handler(new Request(`${origin}/api/auth/sign-in/email`, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "ployz-cli/0.0.0" },
    body: JSON.stringify({ email: `${name}@example.test`, password: "correct-horse-battery-staple" }),
  }));
  assert.strictEqual(response.status, 200);
  return { session: response.headers.get("set-auth-token") ?? assert.fail("no session token") };
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

        yield* database.drizzle.update(organizationToken)
          .set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(organizationToken.id, token.id));
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: token.secret })).status, 401);
        assert.isTrue((yield* cli("GET", "tokens", alice)).json.tokens?.[0]?.expired);

        const second = (yield* cli("POST", "tokens", alice, { name: "ci-2", expires_in_days: 1 })).json.token
          ?? assert.fail("no token");
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: second.secret })).status, 200);
        const [membership] = yield* database.drizzle.delete(member)
          .where(eq(member.organizationId, alice.organization.id)).returning();
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: second.secret })).status, 401);
        // A session whose active Organization is no longer the user's is refused too.
        assert.strictEqual((yield* cli("GET", "tokens", alice)).status, 403);
        yield* database.drizzle.insert(member).values(membership ?? assert.fail("no membership"));

        const removed = yield* cli("DELETE", `tokens/${second.id}`, alice);
        assert.deepStrictEqual(removed.json.removed, { id: second.id, kind: "token" });
        assert.strictEqual((yield* cli("GET", "tokens", { bearer: second.secret })).status, 401);
        assert.strictEqual((yield* cli("DELETE", `tokens/${second.id}`, alice)).status, 404);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "POST fleet-tokens discloses a pflt_ token once and stores only its hash",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");

        assert.strictEqual((yield* cli("POST", "fleet-tokens", alice, { expires_in: "2d" })).status, 422);
        assert.strictEqual((yield* cli("POST", "fleet-tokens", alice, { name: "Bad Name", expires_in: "1d" })).status, 422);
        assert.strictEqual((yield* cli("POST", "fleet-tokens", alice,
          { name: "00000000-0000-4000-8000-000000000001", expires_in: "1d" })).status, 422);

        const made = yield* cli("POST", "fleet-tokens", alice, { expires_in: "7d" });
        assert.strictEqual(made.status, 200);
        const created = made.json.fleet_token ?? assert.fail("no fleet token");
        assert.match(created.token, /^pflt_[\w-]{43}$/);
        assert.match(created.name, /^[a-z]+-[a-z]+$/);

        const rows = yield* database.drizzle.select().from(fleetToken);
        assert.deepStrictEqual(rows.map((row) => [row.name, row.tokenHash]), [
          [created.name, createHash("sha256").update(created.token).digest("hex")],
        ]);
        assert.notInclude(JSON.stringify(rows), created.token);

        const named = yield* cli("POST", "fleet-tokens", alice, { name: "rack-1", expires_in: "1d", auto_enroll_max: 3 });
        assert.strictEqual(named.json.fleet_token?.name, "rack-1");
        assert.strictEqual((yield* cli("POST", "fleet-tokens", alice, { name: "rack-1", expires_in: "1d" })).status, 409);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "GET fleet-tokens lists the Organization's unrevoked tokens without their secrets",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const first = (yield* cli("POST", "fleet-tokens", alice, { name: "rack-1", expires_in: "1d" })).json.fleet_token
          ?? assert.fail("no fleet token");
        yield* cli("POST", "fleet-tokens", alice, { name: "rack-2", expires_in: "30d", auto_enroll_max: 5 });
        yield* database.drizzle.update(fleetToken)
          .set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(fleetToken.id, first.id));

        const listed = yield* cli("GET", "fleet-tokens", alice);
        assert.notInclude(listed.text, first.token);
        assert.deepStrictEqual(
          listed.json.fleet_tokens?.map((row) => [row.name, row.expired, row.auto_enroll_max]),
          [["rack-1", true, null], ["rack-2", false, 5]],
        );
        assert.deepStrictEqual((yield* cli("GET", "fleet-tokens", bob)).json.fleet_tokens, []);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "DELETE fleet-tokens revokes by id or name, frees the name, and refuses another Organization's token",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const made = (yield* cli("POST", "fleet-tokens", alice, { name: "rack-1", expires_in: "1d" })).json.fleet_token
          ?? assert.fail("no fleet token");

        assert.strictEqual((yield* cli("DELETE", `fleet-tokens/${made.id}`, bob)).status, 404);
        assert.strictEqual((yield* cli("DELETE", "fleet-tokens/rack-1", bob)).status, 404);
        const [untouched] = yield* database.drizzle.select().from(fleetToken).where(eq(fleetToken.id, made.id));
        assert.isNull(untouched?.revokedAt);

        const revoked = yield* cli("DELETE", "fleet-tokens/rack-1", alice);
        assert.strictEqual(revoked.status, 200);
        assert.strictEqual(revoked.json.waiting_removed, 0);
        assert.deepStrictEqual((yield* cli("GET", "fleet-tokens", alice)).json.fleet_tokens, []);
        assert.strictEqual((yield* cli("DELETE", `fleet-tokens/${made.id}`, alice)).status, 200);
        assert.strictEqual((yield* cli("DELETE", "fleet-tokens/rack-1", alice)).status, 404);

        const reused = yield* cli("POST", "fleet-tokens", alice, { name: "rack-1", expires_in: "1d" });
        assert.strictEqual(reused.json.fleet_token?.name, "rack-1");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "an Organization Token can't make or revoke tokens or Fleet Tokens, so none outlives the token that made it",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const token = (yield* cli("POST", "tokens", alice, { name: "ci", expires_in_days: 1 })).json.token ?? assert.fail("no token");
        const device = (yield* cli("GET", "tokens", alice)).json.devices?.[0] ?? assert.fail("no device");
        const asToken = { bearer: token.secret };

        assert.strictEqual((yield* cli("POST", "tokens", asToken, { name: "child", expires_in_days: 365 })).status, 422);
        assert.lengthOf(yield* database.drizzle.select().from(organizationToken), 1);
        assert.strictEqual((yield* cli("DELETE", `tokens/${token.id}`, asToken)).status, 422);
        assert.strictEqual((yield* cli("DELETE", `tokens/${device.id}`, asToken)).status, 422);
        assert.strictEqual((yield* cli("GET", "tokens", asToken)).status, 200);
        assert.strictEqual((yield* cli("GET", "tokens", alice)).status, 200);

        const fleet = (yield* cli("POST", "fleet-tokens", alice, { name: "rack-1", expires_in: "1d" })).json.fleet_token ?? assert.fail("no fleet token");
        assert.strictEqual((yield* cli("POST", "fleet-tokens", asToken, { expires_in: "30d" })).status, 422);
        assert.strictEqual((yield* cli("DELETE", `fleet-tokens/${fleet.id}`, asToken)).status, 422);
        assert.deepStrictEqual((yield* database.drizzle.select().from(fleetToken)).map((row) => [row.name, row.revokedAt]), [["rack-1", null]]);
        assert.strictEqual((yield* cli("GET", "fleet-tokens", asToken)).status, 200);

        // A cookie, which any page can make the browser send, signs no CLI request in.
        const forged = new Request(`${origin}/api/cli/tokens`, {
          method: "POST",
          headers: { cookie: alice.cookie, origin: "https://evil.example", "sec-fetch-site": "cross-site" },
          body: JSON.stringify({ name: "forged", expires_in_days: 365 }),
        });
        const refused = yield* handleCliRequest(forged).pipe(
          Effect.map((response) => response instanceof Response ? response.status : 200),
          Effect.catch((error) => Effect.succeed(statusForPublicError(encodePublicError(error)))),
        );
        assert.strictEqual(refused, 401);
        assert.lengthOf(yield* database.drizzle.select().from(organizationToken), 1);
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

/** The bodies `ployz volume mirror|sync|mirror rm|move|release|restore` send, pinned the same in the CLI's `each_kind_posts_only_its_own_fields`. */
const cliVolumeRunBodies: ReadonlyArray<CliBody> = [
  { environment: { project: "shop", environment: "production" }, kind: "mirror", to: "web-2" },
  { environment: { project: "shop", environment: "production" }, kind: "sync", full: true },
  { environment: { project: "shop", environment: "production" }, kind: "delete_mirror", slot: "web-2", confirm: "data" },
  { environment: { project: "shop", environment: "production" }, kind: "move", to: "web-2" },
  { environment: { project: "shop", environment: "production" }, kind: "release" },
  { environment: { project: "shop", environment: "production" }, kind: "restore", from: "web-2" },
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
        const byId = yield* cli("POST", "volumes/vol-1/runs", alice, { environment: "env-1", kind: "sync", full: true });
        assert.strictEqual(byId.status, 422);
        const plain = yield* cli("POST", "volumes/vol-1/runs", alice, { environment: { project: "shop", environment: "production" }, kind: "sync", full: false });
        assert.strictEqual(plain.status, 422);
        const sync = yield* cli("POST", "volumes/vol-1/sync", alice, { environment: { project: "shop", environment: "production" } });
        assert.notStrictEqual(sync.status, 422);
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
  "a dashboard Organization switch moves only the browser's session, so the CLI keeps acting where org use put it",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const browser = yield* auth.resolveActor(new Headers({ cookie: alice.cookie }));
        yield* drizzle.insert(member).values({ userId: browser.userId, organizationId: bob.organization.id });
        const device = yield* signIn("alice");
        const deviceHeaders = new Headers({ authorization: `Bearer ${device.session}` });
        const used = yield* auth.handler(new Request(`${origin}/api/auth/organization/set-active`, {
          method: "POST",
          headers: { authorization: `Bearer ${device.session}`, "content-type": "application/json" },
          body: JSON.stringify({ organizationSlug: bob.organization.slug }),
        }));
        assert.strictEqual(used.status, 200);
        const deviceSessionId = (yield* resolveCaller(deviceHeaders)).credential.id;
        const row = (id: string) => drizzle
          .select({ activeOrganizationId: session.activeOrganizationId, updatedAt: session.updatedAt })
          .from(session).where(eq(session.id, id));
        const deviceBefore = yield* row(deviceSessionId);

        yield* syncOrganizationSlug(browser, { organizationSlug: bob.organization.slug });
        yield* syncOrganizationSlug(browser, { organizationSlug: alice.organization.slug });

        assert.deepStrictEqual(yield* row(deviceSessionId), deviceBefore);
        assert.strictEqual((yield* row(browser.credential.id))[0]?.activeOrganizationId, alice.organization.id);
        assert.strictEqual((yield* resolveCaller(deviceHeaders)).organization.slug, bob.organization.slug);
        // What `ployz mcp login` sends: its agent token lands in the device's Organization, not the browser's.
        const minted = yield* cli("POST", "tokens", device, { name: "agent", kind: "agent", expires_in_days: 30 });
        assert.strictEqual(minted.json.token?.organization, bob.organization.slug);
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
        const first = ids.machineId("00000000000000000000000000001239");
        const later = ids.machineId("00000000000000000000000000001240");
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

        // Rerunning `token rm`, signed back in, retries once the Server is back.
        laterServer.online = true;
        const back = yield* signIn("alice");
        assert.strictEqual((yield* cli("DELETE", `tokens/${device}`, ci2)).status, 422);
        const retried = (yield* cli("DELETE", `tokens/${device}`, back)).json;
        assert.deepStrictEqual(retried.removed, { id: device, kind: "device" });
        assert.deepStrictEqual(retried.servers, { confirmed: [later], unconfirmed: [] });
        assert.isFalse(laterServer.slots.has(serverAccessLabel(device)));
        assert.deepStrictEqual((yield* cli("GET", "tokens", ci2)).json.revoking, []);
        assert.strictEqual((yield* cli("DELETE", `tokens/${device}`, back)).status, 404);

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
  "only an owner or admin gets Server access; a member is refused with no holder, and a demoted admin's holder is "
    + "retired by the sweep",
  () =>
    Effect.gen(function* () {
      const fake = fakeServers();
      const layer = yield* cliLayer(fake.layer);
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const machine = ids.machineId("00000000000000000000000000001241");
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const slug = alice.organization.slug;
        yield* enroll(alice.organization.id, machine, true);
        const server = { online: true, slots: new Map<string, string>() };
        fake.servers.set(machine, server);
        const [personal] = yield* database.drizzle.select({ userId: member.userId }).from(member)
          .where(eq(member.organizationId, bob.organization.id));
        const bobId = personal?.userId ?? assert.fail("no member");
        yield* database.drizzle.insert(member).values({ userId: bobId, organizationId: alice.organization.id, role: "member" });
        yield* database.drizzle.update(session).set({ activeOrganizationId: alice.organization.id, activeOrganizationSlug: slug })
          .where(eq(session.userId, bobId));
        const device = (yield* cli("GET", "tokens", bob)).json.devices?.[0]?.id ?? assert.fail("no device");
        const setRole = (role: "admin" | "member") => database.drizzle.update(member).set({ role })
          .where(and(eq(member.userId, bobId), eq(member.organizationId, alice.organization.id)));

        const refused = yield* cli("POST", "server-access", bob);
        assert.strictEqual(refused.status, 403);
        assert.deepStrictEqual(JSON.parse(refused.text), { error: {
          code: "forbidden",
          message: `Server access needs an admin. Ask an owner or admin of ${slug}.`,
          details: { organization: slug },
        } });
        assert.strictEqual(fake.sets(), 0);
        assert.lengthOf(yield* database.drizzle.select().from(serverAccess), 0);

        yield* setRole("admin");
        const granted = yield* cli("POST", "server-access", bob);
        assert.strictEqual(granted.status, 200);
        const key = granted.json.connections?.[0]?.management ?? assert.fail("no capability");
        assert.strictEqual(server.slots.get(serverAccessLabel(device)), key);

        yield* setRole("member");
        yield* retireServerAccess();
        assert.isFalse(server.slots.has(serverAccessLabel(device)));
        assert.strictEqual((yield* cli("POST", "server-access", bob)).status, 403);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "github lists the caller's installations and a readable repository's branches, and disconnects one",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer(undefined, { env: { PLOYZ_VERIFY_GITHUB_ORIGIN: "http://127.0.0.1:4555" } });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const empty = yield* cli("GET", "github", alice);
        assert.deepInclude(empty.json, { ready: false, installations: [], repositories: [] });
        assert.match(empty.json.connect_url ?? "", /\/api\/github\/connect\?organization=[a-z0-9-]+$/);

        const database = yield* Database;
        const [row] = yield* database.drizzle.select({ userId: member.userId }).from(member)
          .where(eq(member.organizationId, alice.organization.id));
        const userId = row?.userId ?? assert.fail("no member");
        yield* database.drizzle.insert(githubInstallation).values([
          { installationId: 7, accountLogin: "acme", accountType: "Organization" },
          { installationId: 8, accountLogin: "alice", accountType: "User" },
        ]);
        yield* database.drizzle.insert(organizationGithubInstallation).values([
          { organizationId: alice.organization.id, installationId: 7, connectedByUserId: userId, connectedGithubLogin: "alice" },
          { organizationId: alice.organization.id, installationId: 8, connectedByUserId: userId, connectedGithubLogin: "alice" },
        ]);
        yield* database.drizzle.insert(githubRepositoryCache).values({
          installationId: 7, repositoryId: 11, name: "web", fullName: "acme/web", defaultBranch: "main",
          private: true, htmlUrl: "https://github.com/acme/web", repoUpdatedAt: new Date(),
        });
        const connected = yield* cli("GET", "github", alice);
        assert.deepInclude(connected.json, { ready: true });
        assert.deepStrictEqual(
          connected.json.installations?.map(({ id, account, repositories, connected_by }) => ({ id, account, repositories, connected_by })),
          [{ id: 7, account: "acme", repositories: 1, connected_by: "alice" }, { id: 8, account: "alice", repositories: 0, connected_by: "alice" }],
        );

        const branches = yield* cli("GET", "github/branches?repository=ACME/web", alice);
        assert.deepInclude(branches.json, { access: "connected", branches: ["dev", "main"] });
        assert.strictEqual((yield* cli("GET", "github/branches?repository=acme/secret", alice)).status, 404);

        const tree = yield* cli("GET", "github/tree?repository=acme%2Fweb&path=src", alice);
        assert.deepInclude(tree.json, { repository: "acme/web", ref: "main", paths: ["src/main.ts"], truncated: false });
        const dockerfiles = yield* cli("GET", "github/tree?repository=acme%2Fweb&match=**%2FDockerfile", alice);
        assert.deepInclude(dockerfiles.json, { paths: ["Dockerfile"], truncated: false });
        const file = yield* cli("GET", "github/file?repository=acme%2Fweb&path=Dockerfile", alice);
        assert.deepInclude(file.json, { path: "Dockerfile", size: 12, content: "FROM alpine\n" });
        const directory = yield* cli("GET", "github/file?repository=acme%2Fweb&path=src", alice);
        assert.strictEqual(directory.status, 422);
        assert.deepInclude(directory.json.error, { code: "invalid_argument", message: "src is a directory: list it with github tree." });
        // A private repository the Organization can't read answers exactly as one that doesn't exist.
        const hidden = yield* cli("GET", "github/file?repository=acme%2Fsecret&path=Dockerfile", alice);
        const absent = yield* cli("GET", "github/tree?repository=nobody%2Fnothing", alice);
        assert.strictEqual(hidden.status, 404);
        assert.deepStrictEqual([absent.status, absent.text], [hidden.status, hidden.text]);
        assert.deepStrictEqual(absent.json.error, {
          code: "not_found",
          message: "No repository by that name that this Organization can read.",
          details: { valid_children: ["acme/web"] },
        });

        const removed = yield* cli("DELETE", "github/ACME", alice);
        assert.deepStrictEqual(removed.json.disconnected, { id: 7, account: "acme" });
        assert.strictEqual(removed.json.uninstall_url, "http://127.0.0.1:4555/organizations/acme/settings/installations/7");
        assert.strictEqual((yield* cli("DELETE", "github/acme", alice)).status, 404);
        assert.strictEqual((yield* cli("DELETE", "github/7", alice)).status, 404);
        const personal = yield* cli("DELETE", "github/alice", alice);
        assert.strictEqual(personal.json.uninstall_url, "http://127.0.0.1:4555/settings/installations/8");
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
        const machine = ids.machineId("00000000000000000000000000001260");
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
        assert.strictEqual(other.json.error?.code, "invalid_argument");
        // The slug no longer names it; only the ID does.
        assert.strictEqual((yield* cli("DELETE", `organizations/${slug}`, alice)).status, 422);

        // A Project must go first, through its own teardown; nothing is unpaired meanwhile.
        const write = (command: Parameters<typeof callStore>[2]) => callStore(alice.organization.id, userId, command, { source: "dashboard" }, { approval: "not_required", permissions: permissionsOf("owner") });
        const created = yield* write({ operation: "write", command: {
          command: "create_project", id: "00000000-0000-4000-8000-000000001260", name: "shop",
          default_environment: "00000000-0000-4000-8000-000000001261",
        } });
        assert.isTrue(created.ok);
        const refused = yield* cli("DELETE", `organizations/${alice.organization.id}`, alice);
        assert.strictEqual(refused.status, 409);
        assert.deepStrictEqual(refused.json.error?.details?.next, { tool: "remove", input: { project: "shop" } });
        assert.isTrue(server.slots.has("cloud"));
        assert.lengthOf(yield* database.drizzle.select().from(operationApprovals), 0, "a Project refuses it before anyone is asked");
        assert.isTrue((yield* write({ operation: "write", command: { command: "remove", project: "shop" } })).ok);

        // From `ployz`, a human approves it first.
        const approval = yield* approveAsked(yield* cli("DELETE", `organizations/${alice.organization.id}`, alice), userId, alice.organization.id);
        assert.isTrue(server.slots.has("cloud"));

        // A Server offline: the Organization stays, disabled, and says which Server must still confirm.
        server.online = false;
        const partial = yield* cli("DELETE", `organizations/${alice.organization.id}`, { ...alice, approval });
        assert.deepInclude(partial.json, { organization: slug, removed: false, servers: { confirmed: [], unconfirmed: [machine] } });
        assert.lengthOf(yield* database.drizzle.select().from(organization).where(eq(organization.id, alice.organization.id)), 1);
        assert.lengthOf(yield* database.drizzle.select().from(organizationPairing), 1);

        // The same command confirms it once the Server is back, clearing Cloud's key and every device key.
        server.online = true;
        const removed = yield* cli("DELETE", `organizations/${alice.organization.id}`, { ...alice, approval });
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
  "org new makes an Organization the user owns under the first free slug, and moves only the calling session into it",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const laptop = yield* signIn("alice");
        const personal = alice.organization.slug;
        const current = (reply: Reply) => reply.organizations?.find((row) => row.current)?.slug;
        const count = () => database.drizzle.select().from(organization).pipe(Effect.map((rows) => rows.length));
        const before = yield* count();

        const token = (yield* cli("POST", "tokens", alice, { name: "ci", expires_in_days: 1 })).json.token ?? assert.fail("no token");
        const byToken = yield* cli("POST", "organizations", { bearer: token.secret }, { name: "Acme" });
        assert.strictEqual(byToken.status, 422);
        assert.strictEqual(byToken.json.error?.code, "invalid_argument");
        assert.strictEqual((yield* cli("POST", "organizations", alice, { name: " " })).status, 422);
        assert.strictEqual((yield* cli("POST", "organizations", alice, { name: "Acme", slug: "!!!" })).status, 422);
        assert.strictEqual((yield* cli("POST", "organizations", alice, { name: "x".repeat(65) })).status, 422);
        assert.deepStrictEqual(refusedWith(yield* cli("POST", "organizations", alice, { name: "日本" })), [
          422, "This name has no letters or digits to make a slug from. Pass --slug.",
        ]);
        // A sign-in that ended mid-request has no session to move, so its Organization rolls back.
        const [owner] = yield* database.drizzle.select({ userId: member.userId }).from(member)
          .innerJoin(organization, eq(organization.id, member.organizationId)).where(eq(organization.slug, personal));
        const ended = yield* Effect.exit(createOrganizationForSession(owner?.userId ?? assert.fail("no owner"), crypto.randomUUID(), { name: "Gone", slug: "gone" }));
        assert.isTrue(Exit.isFailure(ended) && Cause.squash(ended.cause) instanceof Unauthorized);
        assert.strictEqual(yield* count(), before);

        const made = madeOrganization(yield* cli("POST", "organizations", alice, { name: " Acme Corp " }));
        assert.deepInclude(made, { slug: "acme-corp", name: "Acme Corp" });
        assert.strictEqual(current((yield* cli("GET", "organizations", alice)).json), "acme-corp");
        // The user's other sign-in keeps acting where it was.
        assert.strictEqual(current((yield* cli("GET", "organizations", laptop)).json), personal);

        const suffixed = madeOrganization(yield* cli("POST", "organizations", laptop, { name: "Acme again", slug: "Acme Corp" }));
        assert.match(suffixed.slug, /^acme-corp-[0-9a-f]{8}$/);
        assert.strictEqual(current((yield* cli("GET", "organizations", laptop)).json), suffixed.slug);
        assert.strictEqual(current((yield* cli("GET", "organizations", alice)).json), "acme-corp");

        const roles = yield* database.drizzle.select({ slug: organization.slug, role: member.role }).from(member)
          .innerJoin(organization, eq(organization.id, member.organizationId))
          .orderBy(organization.slug);
        assert.deepStrictEqual(roles.map((row) => [row.slug.replace(/-[0-9a-f]{8}$/, "-*"), row.role]), [
          ["acme-corp", "owner"], ["acme-corp-*", "owner"], [personal, "owner"],
        ]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "org rm is an owner's alone: an admin or a member is refused and the Organization stays",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const slug = alice.organization.slug;
        const [personal] = yield* database.drizzle.select({ userId: member.userId }).from(member)
          .where(eq(member.organizationId, bob.organization.id));
        const bobId = personal?.userId ?? assert.fail("no member");
        yield* database.drizzle.insert(member).values({ userId: bobId, organizationId: alice.organization.id, role: "admin" });
        yield* database.drizzle.update(session).set({ activeOrganizationId: alice.organization.id, activeOrganizationSlug: slug })
          .where(eq(session.userId, bobId));
        for (const role of ["admin", "member"] as const) {
          yield* database.drizzle.update(member).set({ role })
            .where(and(eq(member.userId, bobId), eq(member.organizationId, alice.organization.id)));
          const refused = yield* cli("DELETE", `organizations/${alice.organization.id}`, bob);
          assert.strictEqual(refused.status, 403);
          assert.strictEqual(refused.json.error?.code, "forbidden");
          assert.lengthOf(yield* database.drizzle.select().from(organization).where(eq(organization.id, alice.organization.id)), 1);
        }
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "org rm and forgetting every Server wait on a human for a CLI or agent token, and ask nothing from the dashboard",
  () =>
    Effect.gen(function* () {
      const fake = fakeServers();
      const layer = yield* cliLayer(fake.layer);
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const ownerOf = Effect.fn(function* (organizationId: string) {
          const [owner] = yield* drizzle.select({ userId: member.userId }).from(member).where(eq(member.organizationId, organizationId));
          return owner?.userId ?? assert.fail("no member");
        });
        /** An Organization whose one Server is gone, so forgetting it is all that's left before its removal. */
        const goneServer = Effect.fn(function* (organizationId: string, machine: MachineId) {
          yield* enroll(organizationId, machine, true);
          yield* drizzle.update(organizationPairing).set({ founderMachineId: machine }).where(eq(organizationPairing.organizationId, organizationId));
          fake.servers.set(machine, { online: false, slots: new Map() });
        });
        const pairings = (organizationId: string) =>
          drizzle.select().from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
        const organizations = (organizationId: string) => drizzle.select().from(organization).where(eq(organization.id, organizationId));

        const ada = yield* signUp("ada");
        const adaId = yield* ownerOf(ada.organization.id);
        yield* goneServer(ada.organization.id, ids.machineId("00000000000000000000000000001270"));
        assert.isTrue((yield* forgetServers({ userId: adaId, organizationId: ada.organization.id })).ok);
        assert.isTrue((yield* removeOrganizationAsMember({ userId: adaId }, ada.organization.slug)).ok);
        assert.lengthOf(yield* organizations(ada.organization.id), 0);
        assert.lengthOf(yield* drizzle.select().from(operationApprovals), 0, "the dashboard's confirm is the approval");

        const alice = yield* signUp("alice");
        const { id: organizationId, slug } = alice.organization;
        const userId = yield* ownerOf(organizationId);
        const machine = ids.machineId("00000000000000000000000000001271");
        yield* goneServer(organizationId, machine);
        const tokens = yield* Effect.forEach(["cli", "agent"] as const, (kind) => cli("POST", "tokens", alice, { name: kind, expires_in_days: 1, kind }).pipe(
          Effect.map((made) => made.json.token ?? assert.fail("no token")),
        ));
        const asked = (reply: { json: Reply }) => {
          const details = reply.json.error?.details;
          return [reply.json.error?.code, details?.operation, details?.effects?.map(({ kind, name }) => [kind, name])];
        };
        for (const token of tokens) {
          const forget = yield* cli("POST", "forget-servers", { bearer: token.secret }, { organization: slug });
          assert.deepStrictEqual(asked(forget), ["approval_required", { verb: "forget", name: slug }, [["removes_server", machine]]], token.kind);
          const remove = yield* cli("DELETE", `organizations/${organizationId}`, { bearer: token.secret });
          assert.deepStrictEqual(asked(remove), ["approval_required", { verb: "remove", name: slug }, [["removes_organization", slug]]], token.kind);
        }
        assert.lengthOf(yield* pairings(organizationId), 1);
        assert.lengthOf(yield* organizations(organizationId), 1);

        const [cliToken] = tokens;
        const bearer = cliToken?.secret ?? assert.fail("no cli token");
        const forgetting = yield* approveAsked(yield* cli("POST", "forget-servers", { bearer }, { organization: slug }), userId, organizationId);
        const forgotten = yield* cli("POST", "forget-servers", { bearer, approval: forgetting }, { organization: slug });
        assert.deepInclude(forgotten.json, { organization: slug, servers: [{ id: machine, name: machine, reach: "didnt_answer" }] }, forgotten.text);
        assert.lengthOf(yield* pairings(organizationId), 0);
        const removing = yield* approveAsked(yield* cli("DELETE", `organizations/${organizationId}`, { bearer }), userId, organizationId);
        assert.deepInclude((yield* cli("DELETE", `organizations/${organizationId}`, { bearer, approval: removing })).json, { organization: slug, removed: true });
        assert.lengthOf(yield* organizations(organizationId), 0);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** Every route better-auth 1.7.6's organization plugin mounts with Ployz's options, but set-active, `ployz org use`. */
const CLOSED_ORGANIZATION_ROUTES = [
  "accept-invitation", "cancel-invitation", "check-slug", "create", "delete", "get-active-member", "get-active-member-role",
  "get-full-organization", "get-invitation", "get-organization", "has-permission", "invite-member", "leave", "list",
  "list-invitations", "list-members", "list-user-invitations", "reject-invitation", "remove-member", "update",
  "update-member-role",
];

it.live(
  "better-auth's organization routes but set-active answer 404 to a signed-in member, so membership changes only go through Ployz",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const alice = yield* signUp("alice");
        const body = JSON.stringify({ organizationId: alice.organization.id, email: "eve@example.test", role: "owner" });
        const call = (method: "GET" | "POST", path: string) => {
          const init: RequestInit = { method, headers: { cookie: alice.cookie, "content-type": "application/json", origin } };
          if (method === "POST") init.body = body;
          return auth.handler(new Request(`${origin}/api/auth${path}`, init));
        };
        assert.strictEqual((yield* call("GET", "/get-session")).status, 200);
        for (const route of CLOSED_ORGANIZATION_ROUTES) {
          for (const path of [`/organization/${route}`, `/organization/${route}/`]) {
            assert.strictEqual((yield* call("POST", path)).status, 404, `POST ${path}`);
            assert.strictEqual((yield* call("GET", path)).status, 404, `GET ${path}`);
          }
        }
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
          const bearer = as.session ?? as.bearer;
          if (bearer !== undefined) headers.set("authorization", `Bearer ${bearer}`);
          const request = new Request(`${origin}/api/cli/servers/enroll`, { method: "POST", headers, body: JSON.stringify(body) });
          return handleOrganizationCliRequest(request, CliMintMachineEnrollmentInput, mintCliMachineEnrollment).pipe(
            Effect.map((response) => response.status),
            Effect.catch((error) => Effect.succeed(statusForPublicError(encodePublicError(error)))),
          );
        };
        assert.strictEqual(yield* enroll(alice, { organizationId: alice.organization.id }), 200);
        assert.strictEqual(yield* enroll({ bearer: token.secret }, { organizationId: alice.organization.id }), 200);
        assert.strictEqual(yield* enroll({ bearer: token.secret }, { organizationId: bob.organization.id }), 403);
        assert.strictEqual(yield* enroll(alice, { organizationId: bob.organization.id }), 404);
        assert.strictEqual(yield* enroll({}, { organizationId: alice.organization.id }), 401);
        assert.strictEqual(yield* enroll(alice, { organizationId: alice.organization.id, extra: true }), 422);
        assert.strictEqual(yield* enroll(alice, { organizationId: "not-a-uuid" }), 422);
        // The slug no longer names an Organization to the CLI surface.
        assert.strictEqual(yield* enroll(alice, { organizationSlug: alice.organization.slug }), 422);
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
          via: "cli", credentialKind: "session", credentialId: "device", command: "publish", digest: "7:abc",
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

const fra1 = ids.machineId("f");
const leftBehind = { kind: "docker_volume" as const, id: { machine_id: fra1, name: "left-behind_data" as DockerVolumeName } };

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
        assert.strictEqual(pending.json.error?.details?.approval_id, approvalId);

        const approved = yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: details.approval ?? "" } });
        assert.strictEqual(approved.status, 200);
        assert.strictEqual(approved.json.approval?.status, "approved");

        const started = yield* cli("POST", "namespaces/left-behind/clean", { ...alice, approval: approvalId });
        assert.strictEqual(started.status, 202);
        const cleanupId = started.json.id ?? assert.fail("no clean id");
        assert.deepStrictEqual((yield* cli("GET", `namespace-cleanups/${cleanupId}`, alice)).json, { state: "pending" });
        const again = yield* cli("POST", "namespaces/left-behind/clean", { ...alice, approval: approvalId });
        assert.strictEqual(again.json.id, cleanupId);
        assert.deepStrictEqual(sent.mock.calls.map(([event]) => (event as { id?: string }).id), [`namespace-cleanup-${cleanupId}`]);
        assert.strictEqual((yield* cli("GET", "namespace-cleanups/not-a-uuid", alice)).json.error?.code, "not_found");

        const removal = yield* cli("POST", `servers/${fra1}/forget`, alice, {});
        assert.strictEqual(removal.status, 409);
        assert.deepStrictEqual(removal.json.error?.details?.operation, { verb: "forget", name: "fra-1" });

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
  "an agent token's destroy asks whatever header it sends, and only its person answers",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-agent-token-test" });
      vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindClusterOf({ observesFra1: true, sessionsOpened: 0 }), inngest });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        const mint = Effect.fn(function* (body: CliBody) {
          const made = yield* cli("POST", "tokens", alice, { expires_in_days: 30, ...body });
          return (made.json.token ?? assert.fail(made.text)).secret;
        });
        assert.strictEqual((yield* cli("POST", "tokens", alice, { name: "x", expires_in_days: 30, kind: "dashboard" })).status, 422);
        const agent = yield* mint({ name: "mcp", kind: "agent" });
        const ci = yield* mint({ name: "ci" });
        const byAgent = yield* cli("POST", "tokens", { ...alice, via: "agent" }, { name: "sneaky", expires_in_days: 30, kind: "cli" });
        assert.strictEqual(byAgent.json.token?.kind, "agent", byAgent.text);

        const listed = yield* cli("GET", "tokens", alice);
        assert.deepStrictEqual(listed.json.tokens?.map(({ kind }) => kind).sort(), ["agent", "agent", "cli"]);

        const asked = yield* cli("POST", "namespaces/left-behind/clean", { bearer: agent });
        assert.deepStrictEqual([asked.status, asked.json.error?.code], [409, "approval_required"], asked.text);
        const details = asked.json.error?.details ?? assert.fail("no details");
        const approvalId = details.approval_id ?? assert.fail("no approval id");
        const row = () => drizzle.select({ via: operationApprovals.via, status: operationApprovals.status })
          .from(operationApprovals).where(eq(operationApprovals.id, approvalId));
        assert.deepStrictEqual(yield* row(), [{ via: "agent", status: "pending" }]);

        const approve = { approve: { digest: details.approval ?? "" } };
        for (const as of [{ bearer: agent }, { bearer: agent, via: "cli" }, { bearer: ci, via: "agent" }]) {
          for (const body of [approve, { reject: {} }]) {
            const refused = yield* cli("POST", `approvals/${approvalId}`, as, body);
            assert.deepStrictEqual([refused.status, refused.json.error?.message], [403, "Agents can't answer approvals."], refused.text);
          }
        }
        assert.deepStrictEqual(yield* row(), [{ via: "agent", status: "pending" }]);

        const approved = yield* cli("POST", `approvals/${approvalId}`, { bearer: ci }, approve);
        assert.deepStrictEqual([approved.status, approved.json.approval?.status], [200, "approved"], approved.text);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a token is refused what its grant leaves out and any Environment outside it, on every CLI route, and nothing changes",
  () =>
    Effect.gen(function* () {
      const fake = fakeServers();
      const layer = yield* cliLayer(fake.layer, { runtime: leftBehindCluster });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        const mint = Effect.fn(function* (body: CliBody) {
          const made = yield* cli("POST", "tokens", alice, { name: "t", expires_in_days: 30, ...body });
          return made.json.token ?? assert.fail(made.text);
        });
        const reader = (yield* mint({ preset: "read" })).secret;
        const deployer = yield* mint({});
        yield* drizzle.update(organizationToken)
          .set({ permissions: [...permissionsOf("owner").filter((held) => held.endsWith(":read")), "changes:stage", "changes:deploy", "services:control"], environments: ["staging"] })
          .where(eq(organizationToken.id, deployer.id));
        // Holds each Organization-wide write below, but only for staging: none of them lands in an Environment.
        const scoped = yield* mint({});
        yield* drizzle.update(organizationToken)
          .set({ permissions: ["github:connect", "approvals:answer", "servers:shell"], environments: ["staging"] })
          .where(eq(organizationToken.id, scoped.id));

        const [owner] = yield* drizzle.select({ userId: member.userId }).from(member).where(eq(member.organizationId, alice.organization.id));
        yield* drizzle.insert(githubInstallation).values({ installationId: 7, accountLogin: "acme", accountType: "Organization" });
        yield* drizzle.insert(organizationGithubInstallation).values({
          organizationId: alice.organization.id, installationId: 7, connectedByUserId: owner?.userId ?? assert.fail("no owner"), connectedGithubLogin: "alice",
        });
        for (const bearer of [reader, deployer.secret]) {
          assert.deepStrictEqual(refusedWith(yield* cli("DELETE", "github/acme", { bearer })), [403, "This token's grant doesn't include github:connect."]);
        }
        assert.deepStrictEqual(refusedWith(yield* cli("DELETE", "github/acme", { bearer: scoped.secret })), [403, "This token can deploy only to staging."]);
        assert.lengthOf(yield* drizzle.select().from(organizationGithubInstallation), 1);

        const production = { project: "shop", environment: "production" };
        assert.deepStrictEqual(refusedWith(yield* cli("POST", "volumes/vol-1/sync", { bearer: reader }, { environment: production })),
          [403, "This token's grant doesn't include services:control."]);
        assert.deepStrictEqual(refusedWith(yield* cli("POST", "volumes/vol-1/sync", { bearer: deployer.secret }, { environment: production })),
          [403, "This token can deploy only to staging."]);
        for (const body of cliVolumeRunBodies.filter(({ kind }) => kind !== "delete_mirror")) {
          assert.deepStrictEqual(refusedWith(yield* cli("POST", "volumes/vol-1/runs", { bearer: reader }, body)),
            [403, "This token's grant doesn't include services:control."], JSON.stringify(body));
          assert.deepStrictEqual(refusedWith(yield* cli("POST", "volumes/vol-1/runs", { bearer: deployer.secret }, body)),
            [403, "This token can deploy only to staging."], JSON.stringify(body));
        }
        assert.lengthOf(yield* drizzle.select().from(volumeRun), 0);

        const asked = yield* cli("POST", "namespaces/left-behind/clean", alice);
        const details = asked.json.error?.details ?? assert.fail(asked.text);
        const approvalId = details.approval_id ?? assert.fail("no approval id");
        const approve = { approve: { digest: details.approval ?? "" } };
        for (const body of [approve, { reject: {} }]) {
          assert.deepStrictEqual(refusedWith(yield* cli("POST", `approvals/${approvalId}`, { bearer: reader }, body)),
            [403, "This token's grant doesn't include approvals:answer."]);
          assert.deepStrictEqual(refusedWith(yield* cli("POST", `approvals/${approvalId}`, { bearer: scoped.secret }, body)),
            [403, "This token can deploy only to staging."]);
        }
        assert.deepStrictEqual(yield* drizzle.select({ status: operationApprovals.status }).from(operationApprovals), [{ status: "pending" }]);

        assert.deepStrictEqual(refusedWith(yield* cli("POST", "server-access", { bearer: reader })),
          [403, "This token's grant doesn't include servers:shell."]);
        assert.deepStrictEqual(refusedWith(yield* cli("POST", "server-access", { bearer: scoped.secret })),
          [403, "This token can deploy only to staging."]);
        assert.strictEqual(fake.sets(), 0);
        assert.lengthOf(yield* drizzle.select().from(serverAccess), 0);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a Deploy token names an Environment by its rule, and one a Project here has",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const minted = (environment: string) =>
          cli("POST", "tokens", alice, { name: "deploy", expires_in_days: 30, preset: "deploy", environment });
        assert.deepStrictEqual(refusedWith(yield* minted("Not An Environment")),
          [422, "No Project here has an Environment named Not An Environment."]);
        assert.deepStrictEqual(refusedWith(yield* minted("")), [422, "A Deploy token needs an Environment; pass `--environment`."]);
        const unknown = yield* minted("staging");
        assert.deepStrictEqual(refusedWith(unknown), [422, "No Project here has an Environment named staging."]);
        assert.lengthOf(yield* (yield* Database).drizzle.select().from(organizationToken), 0);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a person forgetting a Server from its dashboard modal starts it at once, and a sidebar agent's forget asks first",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "dashboard-forget-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindCluster, inngest });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const { organization: org } = yield* signUp("alice");
        const [alice] = yield* drizzle.select({ userId: member.userId }).from(member).where(eq(member.organizationId, org.id));
        assert.isDefined(alice);
        const { userId } = alice;
        const caller = (via: Caller["via"]): Caller =>
          ({ userId, organization: { id: org.id, slug: org.slug }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via });

        const agent = yield* forgetServer(caller("agent"), fra1, { key: "agent-forget", approvalId: null });
        assert.strictEqual(agent?.ok, false);
        assert.lengthOf(sent.mock.calls, 0);

        const person = yield* forgetServer(caller("dashboard"), fra1, { key: "modal-forget", approvalId: null });
        assert.strictEqual(person?.ok, true);
        assert.lengthOf(yield* drizzle.select().from(machineRemoveAttempt), 1);
        assert.lengthOf(sent.mock.calls, 1);
        const approvals = yield* drizzle.select({ status: operationApprovals.status }).from(operationApprovals);
        assert.deepStrictEqual(approvals.map(({ status }) => status), ["pending"]);
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
        const asked = (yield* cli("POST", `servers/${fra1}/forget`, alice, {})).json.error?.details ?? assert.fail("no ask");
        const approvalId = asked.approval_id ?? assert.fail("no approval id");
        yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: asked.approval ?? "" } });

        const reset = yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, { confirm_data_loss: { confirmed: [] } });
        assert.strictEqual(reset.json.error?.code, "approval_required");
        assert.notStrictEqual(reset.json.error?.details?.approval_id ?? assert.fail("no approval id"), approvalId);

        const started = yield* cli("POST", `servers/${fra1}/forget`, { ...alice, approval: approvalId }, {});
        const removalId = started.json.id ?? assert.fail("no removal id");
        const resetAfterUse = yield* cli("DELETE", `servers/${fra1}`, { ...alice, approval: approvalId }, { confirm_data_loss: { confirmed: [] } });
        assert.strictEqual(resetAfterUse.json.error?.code, "approval_required");
        const elsewhere = yield* cli("POST", `servers/${"e".repeat(32)}/forget`, { ...alice, approval: approvalId }, {});
        assert.strictEqual(elsewhere.json.error?.code, "not_found");
        const ended = new Date();
        yield* drizzle.update(machineRemoveAttempt).set({
          state: "failed", inngestRunId: "run-1", startedAt: ended, terminalAt: ended, failureCode: "unreachable", failureMessage: "gone",
        }).where(eq(machineRemoveAttempt.id, removalId));
        assert.strictEqual((yield* cli("POST", `servers/${fra1}/forget`, { ...alice, approval: approvalId }, {})).json.id, removalId);
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
        const asked = (yield* cli("POST", `servers/${fra1}/forget`, alice, {})).json.error?.details ?? assert.fail("no ask");
        const approvalId = asked.approval_id ?? assert.fail("no approval id");
        yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: asked.approval ?? "" } });
        const removalId = (yield* cli("POST", `servers/${fra1}/forget`, { ...alice, approval: approvalId }, {})).json.id
          ?? assert.fail("no removal id");
        const ended = new Date();
        yield* drizzle.update(machineRemoveAttempt).set({ state: "succeeded", inngestRunId: "run-1", startedAt: ended, terminalAt: ended })
          .where(eq(machineRemoveAttempt.id, removalId));
        cluster.observesFra1 = false;

        const retried = yield* cli("POST", `servers/${fra1}/forget`, { ...alice, approval: approvalId }, {});
        assert.strictEqual(retried.status, 200);
        assert.strictEqual(retried.json.id, removalId);
        for (const gone of [
          yield* cli("POST", `servers/${fra1}/forget`, alice, {}),
          yield* cli("DELETE", `servers/${fra1}`, alice, { confirm_data_loss: { confirmed: [] } }),
        ]) {
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
  "a clean names the Volumes it would delete before anyone confirms, as its approval does, and plans on one Engine session",
  () =>
    Effect.gen(function* () {
      const cluster: LeftBehind = { observesFra1: true, sessionsOpened: 0 };
      const layer = yield* cliLayer(fakeServers().layer, { runtime: leftBehindClusterOf(cluster) });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const preview = yield* cli("GET", "namespaces/left-behind/clean", alice);
        assert.strictEqual(preview.status, 200);
        assert.deepStrictEqual(preview.json, {
          namespace: "left-behind",
          volumes: [{ ...leftBehind.id, label: "used by web at /data on fra-1" }],
        });
        assert.strictEqual(cluster.sessionsOpened, 1);

        cluster.sessionsOpened = 0;
        const asked = (yield* cli("POST", "namespaces/left-behind/clean", alice)).json.error;
        assert.strictEqual(asked?.code, "approval_required");
        assert.deepStrictEqual(
          asked?.details?.effects?.filter(({ kind }) => kind === "deletes_volume").map(({ name }) => name),
          ["used by web at /data on fra-1"],
        );
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

type FraRunsGlobal = { destroys: boolean; namespace: string };

const operationClusterOf = (plan: FraRunsGlobal) => Layer.succeed(OrganizationRuntime, {
  cancel: () => Effect.void,
  open: () => Effect.succeed({
    status: "connected" as const,
    connected: asTestDouble<PloyzSession>()({
      watchFirstFrame: () => Effect.succeed(asTestDouble<RuntimeWatchView>()({
        machines: [{ machine: { id: fra1, name: "fra-1" } }],
        services: plan.destroys
          ? [{
            identity: `${plan.namespace}/web`,
            service_id: "web",
            containers: [{
              kind: "service_container", machine_id: fra1, runtime: { state: "running" }, created_at_unix_nanos: 1,
              resolved_spec: { mode: { mode: "global" }, volumes: [], mounts: [] },
            }],
          }]
          : [],
      })),
      dataLossIfNamespaceDestroyed: () => Effect.succeed({ data_loss: [] }),
    }),
  }),
});

const operations = [
  { verb: "forget", name: "fra-1", method: "POST", path: `servers/${fra1}/forget`, body: {} },
  { verb: "clean", name: "left-behind", method: "POST", path: "namespaces/left-behind/clean", body: undefined },
  { verb: "drain", name: "fra-1", method: "POST", path: `servers/${fra1}/drain`, body: undefined },
] as const;

const ownNamespace = Effect.fn(function* (organizationId: string, userId: string) {
  const { drizzle } = yield* Database;
  const created = yield* callStore(organizationId, userId, {
    operation: "write",
    command: {
      command: "create_project", id: "00000000-0000-4000-8000-00000000d401", name: "shop",
      default_environment: "00000000-0000-4000-8000-00000000d402",
    },
  }, { source: "dashboard" }, { approval: "not_required", permissions: permissionsOf("owner") });
  assert.isTrue(created.ok);
  yield* drizzle.execute(sql`insert into config_namespace (organization_id, namespace, environment_id)
    values (${organizationId}, 'shop-production', '00000000-0000-4000-8000-00000000d402')`);
  const owned = yield* readStore(organizationId, { query: "namespaces" });
  return owned.namespaces[0]?.namespace ?? assert.fail("no owned Namespace");
});

const ownerOf = Effect.fn(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [owner] = yield* drizzle.select({ userId: member.userId }).from(member).where(eq(member.organizationId, organizationId));
  return owner?.userId ?? assert.fail("no member");
});

for (const operation of operations) {
  it.live(
    `a ${operation.verb} that names a denied approval refuses with its reason, even once its plan destroys nothing`,
    () =>
      Effect.gen(function* () {
        const inngest = new Inngest({ id: `cli-${operation.verb}-denied-test` });
        const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
        const plan: FraRunsGlobal = { destroys: true, namespace: "left-behind" };
        const layer = yield* cliLayer(fakeServers().layer, { runtime: operationClusterOf(plan), inngest });
        yield* Effect.gen(function* () {
          const alice = yield* signUp("alice");
          if (operation.verb === "drain") plan.namespace = yield* ownNamespace(alice.organization.id, yield* ownerOf(alice.organization.id));

          for (const reason of ["declined", "cancelled"]) {
            plan.destroys = true;
            const asked = yield* cli(operation.method, operation.path, alice, operation.body);
            assert.strictEqual(asked.status, 409, reason);
            const approvalId = asked.json.error?.details?.approval_id ?? assert.fail("no approval id");
            const denied = yield* cli("POST", `approvals/${approvalId}`, alice, { reject: { reason } });
            assert.strictEqual(denied.json.approval?.status, "denied", reason);
            const approvedLate = yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: asked.json.error?.details?.approval ?? "" } });
            assert.deepStrictEqual(
              [approvedLate.status, approvedLate.json.error?.code, approvedLate.json.error?.message],
              [409, "conflict", "This approval was already decided elsewhere (denied)."],
              reason,
            );

            const plans = operation.verb === "forget" ? [true] : [true, false];
            for (const destroys of plans) {
              plan.destroys = destroys;
              const retried = yield* cli(operation.method, operation.path, { ...alice, approval: approvalId }, operation.body);
              assert.deepStrictEqual(
                [retried.status, retried.json.error?.code, retried.json.error?.message],
                [403, "approval_denied", `A human denied the request to ${operation.verb} ${operation.name}: ${reason}`],
                `${reason}, destroys ${destroys}`,
              );
            }
          }
          assert.lengthOf(sent.mock.calls, 0);
        }).pipe(Effect.provide(layer));
      }),
    60_000,
  );
}

it.live(
  "a CLI drain that loses the last copy of a Global asks, and an approved retry starts one drain",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-drain-global-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const plan: FraRunsGlobal = { destroys: true, namespace: "left-behind" };
      const layer = yield* cliLayer(fakeServers().layer, { runtime: operationClusterOf(plan), inngest });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        plan.namespace = yield* ownNamespace(alice.organization.id, yield* ownerOf(alice.organization.id));
        const drain = operations[2];

        const asked = yield* cli(drain.method, drain.path, alice);
        assert.deepStrictEqual([asked.status, asked.json.error?.code], [409, "approval_required"], asked.text);
        const details = asked.json.error?.details ?? assert.fail("no details");
        assert.deepStrictEqual(details.operation, { verb: "drain", name: "fra-1" });
        assert.isNotEmpty(details.effects ?? []);
        assert.lengthOf(sent.mock.calls, 0);

        const approvalId = details.approval_id ?? assert.fail("no approval id");
        const approved = yield* cli("POST", `approvals/${approvalId}`, alice, { approve: { digest: details.approval ?? "" } });
        assert.strictEqual(approved.json.approval?.status, "approved");
        const started = yield* cli(drain.method, drain.path, { ...alice, approval: approvalId });
        assert.strictEqual(started.status, 202, started.text);
        assert.lengthOf(sent.mock.calls, 1);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a CLI drain that loses no Global's last copy goes ahead without asking",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-drain-quiet-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const plan: FraRunsGlobal = { destroys: false, namespace: "left-behind" };
      const layer = yield* cliLayer(fakeServers().layer, { runtime: operationClusterOf(plan), inngest });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        plan.namespace = yield* ownNamespace(alice.organization.id, yield* ownerOf(alice.organization.id));
        const drained = yield* cli("POST", `servers/${fra1}/drain`, alice);
        assert.strictEqual(drained.status, 202, drained.text);
        assert.lengthOf(sent.mock.calls, 1);
        assert.lengthOf(yield* drizzle.select({ id: operationApprovals.id }).from(operationApprovals), 0);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a request key admits one operation: a replay answers its attempt, another request under it conflicts, a malformed one refuses",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-request-key-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const layer = yield* cliLayer(fakeServers().layer, { runtime: operationClusterOf({ destroys: false, namespace: "left-behind" }), inngest });
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const keyed = { ...alice, key: "clean-left-behind-1" };

        const first = yield* cli("POST", "namespaces/left-behind/clean", keyed);
        assert.strictEqual(first.status, 202, first.text);
        const replayed = yield* cli("POST", "namespaces/left-behind/clean", keyed);
        assert.deepStrictEqual([replayed.status, replayed.json.id], [202, first.json.id]);
        assert.lengthOf(sent.mock.calls, 1);

        const other = yield* cli("POST", "namespaces/elsewhere/clean", keyed);
        assert.deepStrictEqual([other.status, other.json.error?.code], [409, "conflict"]);
        const malformed = yield* cli("POST", "namespaces/left-behind/clean", { ...alice, key: "short" });
        assert.deepStrictEqual([malformed.status, malformed.json.error?.code], [422, "invalid_argument"]);
        assert.lengthOf(sent.mock.calls, 1);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "an operation naming a pending approval still waits on it, one naming none asks under the same approval, and an emptied plan runs",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "cli-pending-test" });
      const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const plan: FraRunsGlobal = { destroys: true, namespace: "left-behind" };
      const layer = yield* cliLayer(fakeServers().layer, { runtime: operationClusterOf(plan), inngest });
      yield* Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const alice = yield* signUp("alice");
        const removal = operations[0];
        const clean = operations[1];
        const removalId = (yield* cli(removal.method, removal.path, alice, removal.body)).json.error?.details?.approval_id
          ?? assert.fail("no approval id");
        const cleanId = (yield* cli(clean.method, clean.path, alice)).json.error?.details?.approval_id
          ?? assert.fail("no approval id");
        const waiting = yield* cli(removal.method, removal.path, { ...alice, approval: removalId }, removal.body);
        assert.deepStrictEqual([waiting.status, waiting.json.error?.details?.approval_id], [409, removalId]);
        assert.lengthOf(sent.mock.calls, 0);

        const unnamed = yield* cli(removal.method, removal.path, alice, removal.body);
        assert.deepStrictEqual([unnamed.status, unnamed.json.error?.details?.approval_id], [409, removalId]);

        plan.destroys = false;
        const emptied = yield* cli(clean.method, clean.path, { ...alice, approval: cleanId });
        assert.strictEqual(emptied.status, 202, emptied.text);
        const again = yield* cli(clean.method, clean.path, alice);
        assert.strictEqual(again.status, 202, again.text);
        assert.strictEqual(again.json.id, emptied.json.id);
        assert.lengthOf(sent.mock.calls, 1);

        const approvals = yield* drizzle.select({ id: operationApprovals.id, status: operationApprovals.status }).from(operationApprovals);
        assert.sameDeepMembers(approvals, [{ id: removalId, status: "pending" }, { id: cleanId, status: "pending" }]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "POST enrollments/plan answers what POST enrollments then starts, writes nothing, and plans only the caller's own servers",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");
        const fleet = (yield* cli("POST", "fleet-tokens", alice, { name: "rack-1", expires_in: "1d" })).json.fleet_token
          ?? assert.fail("no fleet token");
        const report = {
          hostname: "web", os: { id: "ubuntu", version: "24.04" }, kernel: "6.8.0", arch: "x86_64", virt: "kvm", cpuCount: 4,
          memoryTotalBytes: 8_589_934_592, disks: [], docker: { kind: "absent" as const }, cgroup: "v2" as const,
          zfs: { kind: "eligible" as const, targets: [{ kind: "main_disk" as const }] }, blockers: [], warnings: [], cliVersion: "0.3.0",
        };
        const hosts = yield* database.drizzle.insert(waitingServer).values(["web-a", "web-b", "web-c"].map((hostname, index) => ({
          organizationId: alice.organization.id,
          fleetTokenId: fleet.id,
          checkinKey: String(index).repeat(64),
          report: { ...report, hostname },
          observedIp: "203.0.113.7",
        }))).returning({ id: waitingServer.id, hostname: sql<string>`${waitingServer.report}->>'hostname'` });
        const byName = new Map(hosts.map((row) => [row.hostname, row.id]));
        const [a, b, c] = [byName.get("web-a"), byName.get("web-b"), byName.get("web-c")];
        const body = {
          ids: [a, b, c].map((id) => id ?? assert.fail("no host")),
          first: b ?? assert.fail("no host"),
          storage: { kind: "zfs", use_spare_disk: false, fallback_to_docker: false },
          names: { kind: "pattern", pattern: "node-{n}" },
          runs: { services: true, ingress: true, builds: false },
        };

        const planned = yield* cli("POST", "enrollments/plan", alice, body);
        assert.strictEqual(planned.status, 200);
        assert.deepStrictEqual(JSON.parse(planned.text), {
          plan: [
            { waiting_server_id: b, hostname: "web-b", role: "founder", name: "node-1", storage: { kind: "zfs" } },
            { waiting_server_id: a, hostname: "web-a", role: "joiner", name: "node-2", storage: { kind: "zfs" } },
            { waiting_server_id: c, hostname: "web-c", role: "joiner", name: "node-3", storage: { kind: "zfs" } },
          ],
          skipped: [],
          spread: { providers: 1, networks: 1 },
        });
        assert.deepStrictEqual(yield* database.drizzle.select().from(serverEnrollment), []);

        const outsider = yield* cli("POST", "enrollments/plan", bob, body);
        assert.strictEqual(outsider.status, 422);
        assert.notInclude(outsider.text, "web-b");
        assert.strictEqual((yield* cli("POST", "enrollments/other", alice, body)).status, 404);

        const started = yield* cli("POST", "enrollments", alice, body);
        const plan = JSON.parse(planned.text) as { plan: Array<{ waiting_server_id: string; role: string; name: string }> };
        const attempts = (JSON.parse(started.text) as { enrollments: Array<{ id: string; waiting_server_id: string; role: string; name: string }> }).enrollments;
        assert.deepStrictEqual(
          attempts.map((row) => [row.waiting_server_id, row.role, row.name]),
          plan.plan.map((row) => [row.waiting_server_id, row.role, row.name]),
        );
        const listed = JSON.parse((yield* cli("GET", "enrollments", alice)).text) as { enrollments: Array<{ ahead: number | null }> };
        assert.deepStrictEqual(listed.enrollments.map((row) => row.ahead), [null, null, null]);
        const one = JSON.parse((yield* cli("GET", `enrollments/${attempts[0]?.id}`, alice)).text) as { enrollment: { ahead: number | null } };
        assert.strictEqual(one.enrollment.ahead, null);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

const WAITING_FIXTURE = new URL("../../../../../core/crates/ployz/tests/fixtures/waiting-servers.json", import.meta.url);

it.live(
  "GET waiting-servers and GET waiting-servers/:id answer the core fixture ployz parses, so a changed reply cannot ship with a stale fixture",
  () =>
    Effect.gen(function* () {
      const layer = yield* cliLayer();
      yield* Effect.gen(function* () {
        const database = yield* Database;
        const alice = yield* signUp("alice");
        const fleet = (yield* cli("POST", "fleet-tokens", alice, { name: "amber-falcon", expires_in: "1d" })).json.fleet_token
          ?? assert.fail("no fleet token");
        const id = "6b0f3c1e-0000-4000-8000-000000000001";
        yield* database.drizzle.insert(waitingServer).values({
          id,
          organizationId: alice.organization.id,
          fleetTokenId: fleet.id,
          checkinKey: "0".repeat(64),
          report: {
            hostname: "web-1", publicIp: "198.51.100.4", provider: "hetzner", os: { id: "ubuntu", version: "24.04" }, kernel: "6.8.0",
            arch: "x86_64", virt: "kvm", cpuCount: 4, memoryTotalBytes: 8_000_000_000,
            disks: [
              { path: "/dev/sda", sizeBytes: 40_000_000_000, role: "root", availableBytes: 30_000_000_000 },
              { path: "/dev/sdb", byId: "/dev/disk/by-id/scsi-0HC_Volume_1", sizeBytes: 100_000_000_000, role: "empty" },
            ],
            docker: { kind: "present", version: "27.3.1", api: "1.47", compatible: true }, cgroup: "v2",
            zfs: { kind: "eligible", targets: [{ kind: "main_disk" }, { kind: "disk", byId: "/dev/disk/by-id/scsi-0HC_Volume_1" }], buildMinutes: 6 },
            blockers: [{ code: "ports", message: "Port 443 is in use." }], warnings: [{ code: "swap", message: "Swap is on." }],
            prep: { docker: "ready", zfs: "off" }, cliVersion: "0.3.0",
          },
          observedIp: "203.0.113.9",
          firstSeenAt: new Date("2026-10-16T00:00:00.000Z"),
          lastCheckInAt: new Date("2026-10-16T00:05:00.000Z"),
        });
        const list = JSON.parse((yield* cli("GET", "waiting-servers", alice)).text) as unknown;
        const detail = JSON.parse((yield* cli("GET", `waiting-servers/${id}`, alice)).text) as unknown;
        const served = { list, detail };
        if (process.env["PLOYZ_WRITE_WAITING_FIXTURE"] === "1") writeFileSync(WAITING_FIXTURE, `${JSON.stringify(served, null, 2)}\n`);
        const fixture: unknown = JSON.parse(readFileSync(WAITING_FIXTURE, "utf8"));
        assert.deepStrictEqual(served, fixture, "core's waiting-servers.json is stale: run `pnpm waiting:fixture` in dashboard/ and commit it");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
