import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { Cause, ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { Auth, AuthLive, CLI_CLIENT_ID, Unauthorized } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import {
  postgresTestDatabase,
} from "#/test/postgres";

const authLayer = Effect.gen(function* () {
  const testDatabase = yield* postgresTestDatabase;
  const provider = ConfigProvider.fromEnv({
    env: {
      ...testConfigEnvironment(),
      NODE_ENV: "test",
      DATABASE_URL: testDatabase.url.href,
    },
  });
  const configLayer = AppConfig.layer.pipe(
    Layer.provide(ConfigProvider.layer(provider)),
  );
  const databaseLayer = DatabaseLive.pipe(Layer.provide(configLayer));
  return AuthLive.pipe(
    Layer.provide(Layer.mergeAll(
      configLayer,
      databaseLayer,
      Layer.succeed(Polar, { mode: "self_hosted" }),
      Layer.succeed(InngestClient, new Inngest({ id: "auth-test" })),
    )),
  );
});

const origin = "http://localhost:3000";

it.live(
  "resolves one Better Auth session into an explicit Actor",
  () =>
    Effect.gen(function* () {
      const layer = yield* authLayer;

      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const response = yield* auth.handler(
            new Request("http://localhost:3000/api/auth/sign-up/email", {
              method: "POST",
              headers: { "content-type": "application/json" },
              body: JSON.stringify({
                email: "actor@example.test",
                name: "Actor",
                password: "correct-horse-battery-staple",
              }),
            }),
          );
        assert.strictEqual(response.status, 200);
        const cookie = response.headers.get("set-cookie")?.split(";", 1)[0];
        if (cookie === undefined) {
          assert.fail("Better Auth did not set a session cookie");
        }

        const actor = yield* auth.resolveActor(
          new Headers({ cookie }),
        );
        assert.strictEqual(actor.userId.length, 36);
        assert.deepStrictEqual(Object.keys(actor), ["userId"]);

        const restored = yield* auth.getSession(new Headers({ cookie }));
        assert.strictEqual(restored?.session.userId, actor.userId);

        assert.strictEqual(restored?.user.openStartedDeployments, true);
        const userPreference = yield* auth.handler(new Request("http://localhost:3000/api/auth/update-user", {
          method: "POST",
          headers: { cookie, "content-type": "application/json", origin: "http://localhost:3000" },
          body: JSON.stringify({ openStartedDeployments: false }),
        }));
        assert.strictEqual(userPreference.status, 200);
        const updatedUser = yield* auth.getSession(new Headers({ cookie }));
        assert.strictEqual(updatedUser?.user.openStartedDeployments, false);

        const anonymous = yield* Effect.exit(auth.resolveActor(new Headers()));
        assert.strictEqual(anonymous._tag, "Failure");
        if (anonymous._tag === "Failure") {
          assert.instanceOf(Cause.squash(anonymous.cause), Unauthorized);
        }
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** The fields these tests read from better-auth's device-flow and session replies. */
type AuthReply = {
  readonly error?: string;
  readonly device_code?: string;
  readonly user_code?: string;
  readonly verification_uri?: string;
  readonly access_token?: string;
  readonly status?: string;
  readonly client_id?: string;
  readonly user?: { readonly email: string };
  readonly session?: { readonly activeOrganizationId: string | null; readonly activeOrganizationSlug: string | null };
};

type Call = {
  readonly cookie?: string;
  readonly bearer?: string;
  readonly body?: Readonly<Record<string, string>>;
};

const call = Effect.fn(function* (path: string, { cookie, bearer, body }: Call = {}) {
  const auth = yield* Auth;
  // The CLI sends no Origin; only the browser's cookie calls carry one.
  const headers = new Headers();
  if (cookie !== undefined) {
    headers.set("cookie", cookie);
    headers.set("origin", origin);
  }
  if (bearer !== undefined) headers.set("authorization", `Bearer ${bearer}`);
  const init: RequestInit = { headers };
  if (body !== undefined) {
    headers.set("content-type", "application/json");
    init.method = "POST";
    init.body = JSON.stringify(body);
  }
  const response = yield* auth.handler(new Request(`${origin}/api/auth${path}`, init));
  const text = yield* Effect.promise(() => response.text());
  // SAFETY: test-only view of better-auth's JSON; assertions check every field read.
  const json = (text === "" ? null : JSON.parse(text)) as AuthReply | null;
  return { status: response.status, json, cookie: response.headers.get("set-cookie")?.split(";", 1)[0] };
});

const signUp = Effect.fn(function* (name: string) {
  const { cookie } = yield* call("/sign-up/email", {
    body: { email: `${name}@example.test`, name, password: "correct-horse-battery-staple" },
  });
  return cookie ?? assert.fail(`no session cookie for ${name}`);
});

const startDevice = Effect.fn(function* () {
  const started = yield* call("/device/code", { body: { client_id: CLI_CLIENT_ID } });
  assert.strictEqual(started.status, 200);
  const deviceCode = started.json?.device_code ?? assert.fail("no device code");
  const userCode = started.json?.user_code ?? assert.fail("no user code");
  return {
    userCode,
    verificationUri: started.json?.verification_uri,
    poll: () => call("/device/token", { body: {
      grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      device_code: deviceCode,
      client_id: CLI_CLIENT_ID,
    } }),
  };
});

it.live(
  "signs a CLI device in only for the user who claimed its code",
  () =>
    Effect.gen(function* () {
      const layer = yield* authLayer;

      yield* Effect.gen(function* () {
        const owner = yield* signUp("owner");
        const intruder = yield* signUp("intruder");

        const refused = yield* call("/device/code", { body: { client_id: "someone-else" } });
        assert.strictEqual(refused.status, 400);

        const device = yield* startDevice();
        const { userCode } = device;
        assert.strictEqual(device.verificationUri, `${origin}/device`);
        assert.strictEqual((yield* device.poll()).json?.error, "authorization_pending");

        // An unclaimed code can't be approved, even by the user about to claim it.
        const unclaimed = yield* call("/device/approve", { cookie: owner, body: { userCode } });
        assert.strictEqual(unclaimed.status, 400);

        const claimed = yield* call(`/device?user_code=${userCode}`, { cookie: owner });
        assert.strictEqual(claimed.json?.status, "pending");
        assert.strictEqual(claimed.json?.client_id, CLI_CLIENT_ID);

        // A second signed-in user neither takes the claim over nor decides it.
        const looked = yield* call(`/device?user_code=${userCode}`, { cookie: intruder });
        assert.isUndefined(looked.json?.client_id);
        for (const decision of ["/device/approve", "/device/deny"]) {
          const rejected = yield* call(decision, { cookie: intruder, body: { userCode } });
          assert.strictEqual(rejected.status, 403, decision);
        }

        const approved = yield* call("/device/approve", { cookie: owner, body: { userCode } });
        assert.strictEqual(approved.status, 200);
        assert.strictEqual((yield* device.poll()).json?.error, "slow_down");
        // RFC 8628's default five-second polling interval.
        yield* Effect.sleep("5 seconds");
        const granted = yield* device.poll();
        assert.strictEqual(granted.status, 200);
        const token = granted.json?.access_token ?? assert.fail("no access token");

        const session = yield* call("/get-session", { bearer: token });
        assert.strictEqual(session.json?.user?.email, "owner@example.test");
        assert.isString(session.json?.session?.activeOrganizationId);
        assert.isString(session.json?.session?.activeOrganizationSlug);

        // The code is spent; logout ends the device's session.
        assert.strictEqual((yield* device.poll()).json?.error, "invalid_grant");
        const signedOut = yield* call("/sign-out", { bearer: token, body: {} });
        assert.strictEqual(signedOut.status, 200);
        assert.isNull((yield* call("/get-session", { bearer: token })).json);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "lets the owner deny a claimed device code",
  () =>
    Effect.gen(function* () {
      const layer = yield* authLayer;
      yield* Effect.gen(function* () {
        const cookie = yield* signUp("denier");
        const { userCode, poll } = yield* startDevice();
        yield* call(`/device?user_code=${userCode}`, { cookie });
        assert.strictEqual((yield* call("/device/deny", { cookie, body: { userCode } })).status, 200);
        assert.strictEqual((yield* poll()).json?.error, "access_denied");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
