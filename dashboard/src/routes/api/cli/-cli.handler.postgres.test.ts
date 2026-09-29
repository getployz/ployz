import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Cause, ConfigProvider, Effect, Exit, Layer } from "effect";
import { Inngest } from "inngest";
import { organizationBillingState } from "#/modules/billing/tables";
import { Polar, type PolarService } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import { member, organizationToken, session } from "#/modules/identity/tables";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { Auth, AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import { postgresTestDatabase } from "#/test/postgres";

const origin = "http://localhost:3000";

const hostedPolar: PolarService = {
  mode: "hosted",
  productId: "pro",
  listActiveSubscriptions: () => Effect.die("billing reads the cached row"),
  createCheckout: () => Effect.succeed({ url: "https://polar.test/checkout" }),
  createCustomerPortal: () => Effect.succeed({ customerPortalUrl: "https://polar.test/portal" }),
};

const cliLayer = Effect.fn(function* (polar: PolarService, nodeEnv = "test") {
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
