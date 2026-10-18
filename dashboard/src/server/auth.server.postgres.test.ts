import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { Cause, ConfigProvider, Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { Auth, AuthLive, CLI_CLIENT_ID, Unauthorized } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { member, session as sessionTable, user as userTable, verification } from "#/modules/identity/tables";
import { organization as organizationTable } from "#/modules/organization/tables";
import { hashClaimSecret } from "#/modules/identity/claim-required.server";
import { eq, like } from "drizzle-orm";
import { vi } from "vitest";
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
    Layer.provideMerge(Layer.mergeAll(
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
        assert.deepStrictEqual(Object.keys(actor), ["userId", "credential"]);

        const restored = yield* auth.getSession(new Headers({ cookie }));
        assert.strictEqual(restored?.session.userId, actor.userId);
        assert.deepStrictEqual(actor.credential, { kind: "session", id: restored?.session.id });

        assert.strictEqual(restored?.user.openStartedDeployments, true);
        assert.strictEqual(restored?.user.isAnonymous, false);
        const update = (body: Record<string, boolean>) => auth.handler(new Request("http://localhost:3000/api/auth/update-user", {
          method: "POST",
          headers: { cookie, "content-type": "application/json", origin: "http://localhost:3000" },
          body: JSON.stringify(body),
        }));
        const userPreference = yield* update({ openStartedDeployments: false });
        assert.strictEqual(userPreference.status, 200);
        const updatedUser = yield* auth.getSession(new Headers({ cookie }));
        assert.strictEqual(updatedUser?.user.openStartedDeployments, false);
        const anonymityClaimed = yield* update({ isAnonymous: true });
        assert.strictEqual(anonymityClaimed.status, 400);
        assert.strictEqual((yield* auth.getSession(new Headers({ cookie })))?.user.isAnonymous, false);

        const anonymous = yield* Effect.exit(auth.resolveActor(new Headers()));
        assert.strictEqual(anonymous._tag, "Failure");
        if (anonymous._tag === "Failure") {
          assert.instanceOf(Cause.squash(anonymous.cause), Unauthorized);
        }

        const { drizzle } = yield* Database;
        yield* drizzle.update(userTable).set({ isAnonymous: true }).where(eq(userTable.id, actor.userId));
        assert.strictEqual((yield* auth.getSession(new Headers({ cookie })))?.user.isAnonymous, true);
        const agent = yield* Effect.exit(auth.resolveActor(new Headers({ cookie })));
        assert.strictEqual(agent._tag, "Failure");
        if (agent._tag === "Failure") {
          assert.instanceOf(Cause.squash(agent.cause), Unauthorized);
        }
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** The fields these tests read from better-auth's device-flow and session replies. */
const AuthReply = Schema.Struct({
  error: Schema.optionalKey(Schema.String),
  device_code: Schema.optionalKey(Schema.String),
  user_code: Schema.optionalKey(Schema.String),
  verification_uri: Schema.optionalKey(Schema.String),
  access_token: Schema.optionalKey(Schema.String),
  status: Schema.optionalKey(Schema.String),
  client_id: Schema.optionalKey(Schema.String),
  user: Schema.optionalKey(Schema.Struct({ email: Schema.String })),
  session: Schema.optionalKey(Schema.Struct({
    activeOrganizationId: Schema.NullOr(Schema.String), activeOrganizationSlug: Schema.NullOr(Schema.String),
  })),
});
/** Checks the fields the tests read and keeps the rest, so whole-reply assertions still see every field. */
const decodeAuthReply = (text: string) =>
  Schema.decodeUnknownSync(Schema.NullOr(AuthReply))(text === "" ? null : JSON.parse(text), { onExcessProperty: "preserve" });

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
  const json = decodeAuthReply(text);
  return {
    status: response.status,
    json,
    cookie: response.headers.get("set-cookie")?.split(";", 1)[0],
    authToken: response.headers.get("set-auth-token"),
  };
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

        // The device's token signs `ployz` in, never the dashboard, even sent with the dashboard's Origin.
        const auth = yield* Auth;
        const asDashboard = new Headers({ authorization: `Bearer ${token}`, origin });
        assert.isNull(yield* auth.getSession(asDashboard));
        const posing = yield* Effect.exit(auth.resolveActor(asDashboard));
        assert.isTrue(posing._tag === "Failure" && Cause.squash(posing.cause) instanceof Unauthorized);
        assert.strictEqual((yield* auth.getSession(asDashboard, { bearer: true }))?.user.email, "owner@example.test");

        // Nor does any answer to it hand back a session cookie the browser could sign the dashboard in with.
        const slug = session.json?.session?.activeOrganizationSlug ?? assert.fail("no active Organization");
        const moved = yield* call("/organization/set-active", { bearer: token, body: { organizationSlug: slug } });
        assert.strictEqual(moved.status, 200);
        assert.deepStrictEqual([moved.cookie, moved.authToken], [undefined, null]);
        const { drizzle } = yield* Database;
        yield* drizzle.update(sessionTable).set({ expiresAt: new Date(Date.now() + 60_000) }).where(eq(sessionTable.token, token));
        const refreshed = yield* call("/get-session", { bearer: token });
        assert.strictEqual(refreshed.json?.user?.email, "owner@example.test");
        assert.deepStrictEqual([refreshed.cookie, refreshed.authToken], [undefined, null]);

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
  "starts a CLI device in the Organization the approving page named, not the user's first",
  () =>
    Effect.gen(function* () {
      const layer = yield* authLayer;
      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const { drizzle } = yield* Database;
        const cookie = yield* signUp("switcher");
        const browser = (yield* auth.getSession(new Headers({ cookie }))) ?? assert.fail("no browser session");
        const first = browser.session.activeOrganizationId ?? assert.fail("no first Organization");
        const [named] = yield* drizzle.insert(organizationTable).values({ name: "Named", slug: "named" })
          .returning({ id: organizationTable.id });
        const namedId = named?.id ?? assert.fail("no Organization");
        yield* drizzle.insert(member).values({ userId: browser.user.id, organizationId: namedId, role: "member" });
        yield* drizzle.update(sessionTable).set({ activeOrganizationId: namedId, activeOrganizationSlug: "named" })
          .where(eq(sessionTable.id, browser.session.id));

        const { userCode, poll } = yield* startDevice();
        yield* call(`/device?user_code=${userCode}`, { cookie });
        assert.strictEqual((yield* call("/device/approve", { cookie, body: { userCode } })).status, 200);
        const token = (yield* poll()).json?.access_token ?? assert.fail("no access token");

        const device = yield* call("/get-session", { bearer: token });
        assert.notStrictEqual(namedId, first);
        assert.strictEqual(device.json?.session?.activeOrganizationId, namedId);
        assert.deepStrictEqual(
          yield* drizzle.select().from(verification).where(like(verification.identifier, "device-organization:%")),
          [],
        );
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

it.live(
  "sends a new GitHub user back to where they signed in from, and only a plain sign-in to the welcome",
  () =>
    Effect.gen(function* () {
      const layer = yield* authLayer;

      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const { drizzle } = yield* Database;
        const newUserURL = (callbackURL: string) => Effect.gen(function* () {
          yield* drizzle.delete(verification);
          const response = yield* auth.signInGithub(new Headers({ origin }), callbackURL);
          assert.strictEqual(response.status, 200);
          const [row] = yield* drizzle.select({ value: verification.value }).from(verification);
          return (JSON.parse(row?.value ?? "{}") as { newUserURL?: string }).newUserURL;
        });

        assert.strictEqual(yield* newUserURL("/invite/invite-token"), "/invite/invite-token");
        assert.strictEqual(yield* newUserURL("/cloud"), "/cloud?welcome=true");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** GitHub's side of the OAuth dance, answered locally so the real callback runs and signs a new person up. */
const fakeGithub = (login: string, githubId: number) => {
  const real = globalThis.fetch;
  return vi.spyOn(globalThis, "fetch").mockImplementation((input, init) => {
    const url = input instanceof Request ? input.url : String(input);
    if (url.startsWith("https://github.com/login/oauth/access_token")) {
      return Promise.resolve(Response.json({ access_token: `token-${login}`, token_type: "bearer", scope: "read:user,user:email" }));
    }
    if (url === "https://api.github.com/user") {
      return Promise.resolve(Response.json({ id: githubId, login, name: login, email: `${login}@example.test`, avatar_url: null }));
    }
    if (url === "https://api.github.com/user/emails") {
      return Promise.resolve(Response.json([{ email: `${login}@example.test`, primary: true, verified: true }]));
    }
    return real(input, init);
  });
};

it.live(
  "binds the Organization a claim page's GitHub sign-up made to that claim, and nothing for any other sign-up",
  () =>
    Effect.gen(function* () {
      const layer = yield* authLayer;
      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const { drizzle } = yield* Database;
        const signUpWithGithub = (login: string, githubId: number, callbackURL: string) => Effect.gen(function* () {
          const github = fakeGithub(login, githubId);
          try {
            const started = yield* auth.signInGithub(new Headers({ origin }), callbackURL);
            const state = new URL(String((yield* Effect.promise(() => started.json() as Promise<{ url: string }>)).url)).searchParams.get("state");
            const cookie = started.headers.getSetCookie().map((line) => line.split(";", 1)[0]).join("; ");
            const back = yield* auth.handler(new Request(`${origin}/api/auth/callback/github?code=code-${login}&state=${state}`, { headers: { cookie } }));
            assert.strictEqual(back.status, 302, back.headers.get("location") ?? "");
            const [person] = yield* drizzle.select({ id: userTable.id }).from(userTable).where(eq(userTable.email, `${login}@example.test`));
            const [own] = yield* drizzle.select({ organizationId: member.organizationId }).from(member).where(eq(member.userId, person?.id ?? ""));
            return { userId: person?.id ?? assert.fail(`no user for ${login}`), organizationId: own?.organizationId };
          } finally {
            github.mockRestore();
          }
        });
        const bindings = drizzle.select({ identifier: verification.identifier, value: verification.value }).from(verification)
          .where(like(verification.identifier, "claim-sign-in:%"));

        const ada = yield* signUpWithGithub("ada", 1001, "/claim/s3cret");
        assert.deepStrictEqual(yield* bindings, [{ identifier: `claim-sign-in:${ada.userId}:${hashClaimSecret("s3cret")}`, value: ada.organizationId }]);

        const bob = yield* signUpWithGithub("bob", 1002, "/cloud");
        assert.notStrictEqual(bob.userId, ada.userId);
        assert.strictEqual((yield* bindings).length, 1);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "binds each claim sign-up to its own claim when GitHub sign-ins overlap, and nothing to the ordinary ones",
  () =>
    Effect.gen(function* () {
      const layer = yield* authLayer;
      yield* Effect.gen(function* () {
        const auth = yield* Auth;
        const { drizzle } = yield* Database;
        // Routes read the database before they reach Auth, so they resume on a pooled connection. A connection keeps
        // the async context it was opened in, and on a fresh worker the first one opens inside a claim sign-in.
        const afterQuery = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
          drizzle.select({ id: userTable.id }).from(userTable).limit(1).pipe(Effect.andThen(effect));
        const start = (index: number) => Effect.gen(function* () {
          const callbackURL = index % 2 === 0 ? `/claim/overlap-${index}` : "/cloud";
          const started = yield* (index === 0 ? auth.signInGithub(new Headers({ origin }), callbackURL)
            : afterQuery(auth.signInGithub(new Headers({ origin }), callbackURL)));
          const state = new URL(String((yield* Effect.promise(() => started.json() as Promise<{ url: string }>)).url)).searchParams.get("state");
          const cookie = started.headers.getSetCookie().map((line) => line.split(";", 1)[0]).join("; ");
          return { index, state, cookie };
        });
        const flows = [yield* start(0), ...yield* Effect.forEach([1, 2, 3, 4, 5, 6, 7], start, { concurrency: "unbounded" })];
        const real = globalThis.fetch;
        const github = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
          const request = new Request(input, init);
          if (request.url.startsWith("https://github.com/login/oauth/access_token")) {
            return Response.json({ access_token: new URLSearchParams(await request.text()).get("code"), token_type: "bearer", scope: "read:user,user:email" });
          }
          const login = `overlap-${request.headers.get("authorization")?.match(/code-(\d+)/)?.[1]}`;
          const githubId = 2000 + Number(login.slice("overlap-".length));
          if (request.url === "https://api.github.com/user") return Response.json({ id: githubId, login, name: login, email: `${login}@example.test`, avatar_url: null });
          if (request.url === "https://api.github.com/user/emails") return Response.json([{ email: `${login}@example.test`, primary: true, verified: true }]);
          return real(input, init);
        });
        try {
          const callbacks = yield* Effect.forEach(flows, ({ index, state, cookie }) =>
            afterQuery(auth.handler(new Request(`${origin}/api/auth/callback/github?code=code-${index}&state=${state}`, { headers: { cookie } }))), { concurrency: "unbounded" });
          assert.deepStrictEqual(callbacks.map((back) => back.status), flows.map(() => 302));
        } finally {
          github.mockRestore();
        }
        const bound = [];
        const expected = [];
        for (const { index } of flows) {
          const [person] = yield* drizzle.select({ id: userTable.id }).from(userTable).where(eq(userTable.email, `overlap-${index}@example.test`));
          const userId = person?.id ?? assert.fail(`no user for flow ${index}`);
          const bindings = yield* drizzle.select({ identifier: verification.identifier }).from(verification)
            .where(like(verification.identifier, `claim-sign-in:${userId}:%`));
          bound.push({ index, identifiers: bindings.map(({ identifier }) => identifier) });
          expected.push({ index, identifiers: index % 2 === 0 ? [`claim-sign-in:${userId}:${hashClaimSecret(`overlap-${index}`)}`] : [] });
        }
        assert.deepStrictEqual(bound, expected);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
