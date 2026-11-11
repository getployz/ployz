import { assert, describe, it } from "@effect/vitest";
import { Config, ConfigProvider, Effect, Redacted } from "effect";
import { AppConfig, InvalidConfiguration, startupConfigFailure } from "#/server/config.server";

const requiredEnvironment = {
  DATABASE_URL: "postgres://postgres:postgres@localhost:5432/ployz_cloud",
  APP_URL: "http://localhost:3000",
  BETTER_AUTH_SECRET: "better-auth-secret",
  GITHUB_CLIENT_ID: "github-client-id",
  GITHUB_CLIENT_SECRET: "github-client-secret",
  GITHUB_APP_ID: "12345",
  GITHUB_APP_PRIVATE_KEY: "github-app-private-key",
  GITHUB_APP_SLUG: "ployz-test",
  GITHUB_APP_CLIENT_ID: "github-app-client-id",
  GITHUB_APP_CLIENT_SECRET: "github-app-client-secret",
  GITHUB_APP_WEBHOOK_SECRET: "github-app-webhook-secret",
  INNGEST_EVENT_KEY: "inngest-event-key",
  INNGEST_SIGNING_KEY: "inngest-signing-key",
  PLOYZ_HOSTED_DNS_URL: "http://127.0.0.1:9/",
  APP_ENCRYPTION_SECRET: "app-encryption-secret-at-least-32-characters",
};

const hostedPolar = {
  POLAR_ACCESS_TOKEN: "polar-token",
  POLAR_WEBHOOK_SECRET: "polar-webhook-secret",
  POLAR_PRODUCT_ID: "22222222-2222-4222-8222-222222222222",
};

const postShibaMail = {
  POSTSHIBA_API_KEY: "postshiba-api-key",
  POSTSHIBA_TEAM_ID: "team-1",
  POSTSHIBA_CLUSTER_ID: "cluster-1",
  MAIL_FROM: "Ployz <noreply@example.com>",
};

/** The App's key and client secret, which Cloud needs only while it holds the App's authority itself. */
const localAuthority = ["GITHUB_APP_PRIVATE_KEY", "GITHUB_APP_CLIENT_SECRET"];

/** Sign-in through a separate OAuth App, which minter mode requires. */
const OAUTH_CLIENT_ID = "Ov23liAbCdEf0123456789";

const minter = {
  GITHUB_CLIENT_ID: OAUTH_CLIENT_ID,
  GITHUB_MINTER_URL: "https://abc123.lambda-url.eu-central-1.on.aws",
  GITHUB_MINTER_ACCESS_KEY_ID: "AKIAEXAMPLE",
  GITHUB_MINTER_SECRET_ACCESS_KEY: "minter-secret-access-key",
};

const without = (environment: Record<string, string>, name: string) =>
  Object.fromEntries(Object.entries(environment).filter(([key]) => key !== name));

const load = (environment: Record<string, string>) =>
  AppConfig.make.pipe(
    Effect.provideService(
      ConfigProvider.ConfigProvider,
      ConfigProvider.fromEnv({ env: environment }),
    ),
  );

describe("AppConfig", () => {
  it.effect("starts self-hosted without Polar and keeps secrets redacted", () =>
    Effect.gen(function* () {
      const config = yield* load(requiredEnvironment);

      assert.strictEqual(config.app.port, 3000);
      assert.strictEqual(String(config.auth.secret), "<redacted>");
      assert.strictEqual(Redacted.value(config.auth.secret), "better-auth-secret");
      assert.strictEqual(config.github.appSlug, "ployz-test");
      assert.deepStrictEqual(config.polar, { mode: "self_hosted" });
      assert.strictEqual(config.posthog, null);
      assert.strictEqual(config.mail, null);
      assert.strictEqual(config.ployz.hostedDnsUrl.href, "http://127.0.0.1:9/");
    }),
  );

  it.effect("names each missing required variable", () =>
    Effect.gen(function* () {
      for (const name of Object.keys(requiredEnvironment)) {
        const failure = yield* Effect.flip(load(without(requiredEnvironment, name)));
        if (localAuthority.includes(name)) assert.instanceOf(failure, InvalidConfiguration);
        else assert.instanceOf(failure, Config.ConfigError);
        assert.include(String(failure), name);
      }
    }),
  );

  it.effect("names every missing required variable at once", () =>
    Effect.gen(function* () {
      const failure = yield* Effect.flip(load({}));
      assert.instanceOf(failure, Config.ConfigError);
      for (const name of Object.keys(requiredEnvironment).filter((name) => !localAuthority.includes(name))) {
        assert.include(failure.message, name);
      }
    }),
  );

  it.effect("holds the App's key and client secret itself unless a minter holds them", () =>
    Effect.gen(function* () {
      const local = (yield* load(requiredEnvironment)).github.authority;
      assert.strictEqual(local.mode, "local");
      if (local.mode === "local") {
        assert.strictEqual(String(local.appPrivateKey), "<redacted>");
        assert.strictEqual(Redacted.value(local.appClientSecret), "github-app-client-secret");
      }

      const config = yield* load({ ...without(without(requiredEnvironment, "GITHUB_APP_PRIVATE_KEY"), "GITHUB_APP_CLIENT_SECRET"), ...minter });
      assert.deepStrictEqual(Object.keys(config.github).sort(), ["appClientId", "appId", "appSlug", "appWebhookSecret", "authority", "clientId", "clientSecret", "origins"]);
      assert.strictEqual(config.github.authority.mode, "minter");
      if (config.github.authority.mode !== "minter") return;
      assert.strictEqual(config.github.authority.url.origin, minter.GITHUB_MINTER_URL);
      assert.notProperty(config.github.authority, "appPrivateKey");
      assert.notProperty(config.github.authority, "appClientSecret");
      const auth = config.github.authority.auth;
      assert.strictEqual(auth.kind, "sigv4");
      if (auth.kind === "sigv4") {
        assert.strictEqual(auth.region, "eu-central-1");
        assert.strictEqual(String(auth.secretAccessKey), "<redacted>");
      }
    }),
  );

  it.effect("refuses to hold the App's authority twice, or not at all", () =>
    Effect.gen(function* () {
      const keyless = without(without(requiredEnvironment, "GITHUB_APP_PRIVATE_KEY"), "GITHUB_APP_CLIENT_SECRET");
      const refused = (environment: Record<string, string>) => Effect.flip(load(environment)).pipe(Effect.map((failure) => {
        assert.instanceOf(failure, InvalidConfiguration);
        return failure.message;
      }));
      assert.include(yield* refused(keyless), "GITHUB_APP_PRIVATE_KEY");
      assert.include(yield* refused({ ...requiredEnvironment, ...minter }), "must be unset");
      assert.include(yield* refused({ ...keyless, GITHUB_APP_PRIVATE_KEY: "key", ...minter }), "must be unset");
      assert.include(yield* refused({ ...keyless, ...without(minter, "GITHUB_MINTER_URL") }), "need GITHUB_MINTER_URL");
      assert.include(yield* refused({ ...keyless, ...without(minter, "GITHUB_MINTER_SECRET_ACCESS_KEY") }), "needs GITHUB_MINTER_ACCESS_KEY_ID");
      for (const url of ["http://abc123.lambda-url.eu-central-1.on.aws", "https://minter.example.com", "https://abc123.lambda-url.eu-central-1.on.aws.example.com"]) {
        assert.include(yield* refused({ ...keyless, ...minter, GITHUB_MINTER_URL: url }), "Lambda Function URL");
      }
      for (const url of [`${minter.GITHUB_MINTER_URL}/v1`, `${minter.GITHUB_MINTER_URL}/?x=1`]) {
        assert.include(yield* refused({ ...keyless, ...minter, GITHUB_MINTER_URL: url }), "origin");
      }
    }),
  );

  it.effect("signs in through a separate OAuth App in minter mode, judged by sign-in's own client ID", () =>
    Effect.gen(function* () {
      const keyless = without(without(requiredEnvironment, "GITHUB_APP_PRIVATE_KEY"), "GITHUB_APP_CLIENT_SECRET");
      const lab = { NODE_ENV: "development", GITHUB_MINTER_URL: "http://127.0.0.1:8787", PLOYZ_VERIFY_GITHUB_MINTER_BEARER: "lab-bearer", GITHUB_CLIENT_ID: OAUTH_CLIENT_ID };
      const appClientIds = ["Iv1.0123456789abcdef", "Iv23liAbCdEf0123456789", "github-client-id"];
      for (const [path, reach] of [["sigv4", minter], ["bearer", lab]] as const) {
        for (const signIn of ["Ov23liAbCdEf0123456789", "0123456789abcdef0123"]) {
          assert.strictEqual((yield* load({ ...keyless, ...reach, GITHUB_CLIENT_ID: signIn })).github.clientId, signIn, `${path}, ${signIn}`);
        }
        for (const signIn of appClientIds) {
          const failure = yield* Effect.flip(load({ ...keyless, ...reach, GITHUB_CLIENT_ID: signIn }));
          assert.instanceOf(failure, InvalidConfiguration);
          assert.include(failure.message, "GITHUB_CLIENT_ID", `${path}, ${signIn}`);
          assert.include(failure.message, "separate OAuth App");
        }
        // The App's client ID Cloud holds decides nothing: a stale one, or one equal to sign-in's, changes no answer.
        const appSigningIn = { ...keyless, ...reach, GITHUB_CLIENT_ID: "Iv23liAbCdEf0123456789" };
        assert.instanceOf(yield* Effect.flip(load({ ...appSigningIn, GITHUB_APP_CLIENT_ID: "Iv23liStaleStale012345" })), InvalidConfiguration);
        assert.strictEqual((yield* load({ ...keyless, ...reach, GITHUB_APP_CLIENT_ID: OAUTH_CLIENT_ID })).github.authority.mode, "minter");
        for (const padded of [` ${OAUTH_CLIENT_ID}`, `${OAUTH_CLIENT_ID}\n`, `${OAUTH_CLIENT_ID}\u200b`]) {
          assert.notInstanceOf(yield* Effect.flip(load({ ...keyless, ...reach, GITHUB_CLIENT_ID: padded })), InvalidConfiguration);
          assert.include(startupConfigFailure({ ...keyless, ...reach, GITHUB_CLIENT_ID: padded }), "GITHUB_CLIENT_ID");
          assert.include(startupConfigFailure({ ...keyless, ...reach, GITHUB_APP_CLIENT_ID: padded }), "GITHUB_APP_CLIENT_ID");
        }
      }
      const oneApp = { GITHUB_CLIENT_ID: requiredEnvironment.GITHUB_APP_CLIENT_ID };
      assert.strictEqual((yield* load({ ...requiredEnvironment, ...oneApp })).github.authority.mode, "local");
    }),
  );

  it.effect("lets a verify lab reach a local minter with a bearer, outside production only", () =>
    Effect.gen(function* () {
      const keyless = without(without(requiredEnvironment, "GITHUB_APP_PRIVATE_KEY"), "GITHUB_APP_CLIENT_SECRET");
      const lab = { ...keyless, NODE_ENV: "development", GITHUB_MINTER_URL: "http://127.0.0.1:8787", PLOYZ_VERIFY_GITHUB_MINTER_BEARER: "lab-bearer", GITHUB_CLIENT_ID: OAUTH_CLIENT_ID };
      const config = yield* load(lab);
      assert.deepStrictEqual(config.github.authority.mode === "minter" && config.github.authority.auth.kind, "bearer");
      const production = yield* Effect.flip(load({ ...lab, NODE_ENV: "production" }));
      assert.instanceOf(production, InvalidConfiguration);
      assert.include(production.message, "NODE_ENV is production");
      const both = yield* Effect.flip(load({ ...lab, GITHUB_MINTER_ACCESS_KEY_ID: "AKIAEXAMPLE", GITHUB_MINTER_SECRET_ACCESS_KEY: "secret" }));
      assert.instanceOf(both, InvalidConfiguration);
    }),
  );

  it.effect("loads hosted Polar with one product", () =>
    Effect.gen(function* () {
      const config = yield* load({ ...requiredEnvironment, ...hostedPolar });

      assert.strictEqual(config.polar.mode, "hosted");
      if (config.polar.mode === "hosted") {
        assert.strictEqual(config.polar.productId, hostedPolar.POLAR_PRODUCT_ID);
      }
    }),
  );

  it.effect("requires the complete hosted Polar configuration", () =>
    Effect.gen(function* () {
      for (const name of Object.keys(hostedPolar)) {
        const polar = without(hostedPolar, name);
        const failure = yield* Effect.flip(load({ ...requiredEnvironment, ...polar }));
        assert.instanceOf(failure, InvalidConfiguration);
      }
    }),
  );

  it.effect("loads PostShiba mail with its key redacted", () =>
    Effect.gen(function* () {
      const { mail } = yield* load({ ...requiredEnvironment, ...postShibaMail });

      assert.isNotNull(mail);
      if (mail === null) return;
      assert.strictEqual(String(mail.apiKey), "<redacted>");
      assert.strictEqual(Redacted.value(mail.apiKey), "postshiba-api-key");
      assert.deepStrictEqual(
        { teamId: mail.teamId, clusterId: mail.clusterId, from: mail.from },
        { teamId: "team-1", clusterId: "cluster-1", from: "Ployz <noreply@example.com>" },
      );
    }),
  );

  it.effect("requires the complete mail configuration", () =>
    Effect.gen(function* () {
      for (const name of Object.keys(postShibaMail)) {
        const failure = yield* Effect.flip(
          load({ ...requiredEnvironment, ...without(postShibaMail, name) }),
        );
        assert.instanceOf(failure, InvalidConfiguration);
      }
    }),
  );

  it.effect("loads PostHog from its key, on US Cloud unless a host is set", () =>
    Effect.gen(function* () {
      const cloud = yield* load({ ...requiredEnvironment, POSTHOG_KEY: "phc_test" });
      assert.deepStrictEqual(cloud.posthog, { key: "phc_test", host: "https://us.i.posthog.com" });

      const proxied = yield* load({
        ...requiredEnvironment,
        POSTHOG_KEY: "phc_test",
        POSTHOG_HOST: "https://e.example.com/",
      });
      assert.deepStrictEqual(proxied.posthog, { key: "phc_test", host: "https://e.example.com" });
    }),
  );

  it.effect("points every GitHub host at a verify lab's origin, outside production only", () =>
    Effect.gen(function* () {
      const real = yield* load(requiredEnvironment);
      assert.deepStrictEqual(real.github.origins, {
        api: "https://api.github.com", web: "https://github.com", codeload: "https://codeload.github.com",
        oidc: "https://token.actions.githubusercontent.com",
      });

      const lab = { ...requiredEnvironment, NODE_ENV: "development" };
      const fake = yield* load({ ...lab, PLOYZ_VERIFY_GITHUB_ORIGIN: "http://127.0.0.1:4555" });
      assert.deepStrictEqual(fake.github.origins, {
        api: "http://127.0.0.1:4555", web: "http://127.0.0.1:4555", codeload: "http://127.0.0.1:4555", oidc: "http://127.0.0.1:4555",
      });

      const refused = [
        { ...requiredEnvironment, PLOYZ_VERIFY_GITHUB_ORIGIN: "http://127.0.0.1:4555" },
        { ...lab, PLOYZ_VERIFY_GITHUB_ORIGIN: "https://ghe.example.com/api/v3" },
        { ...lab, PLOYZ_VERIFY_GITHUB_ORIGIN: "http://127.0.0.1:4555/?x=1" },
        { ...lab, PLOYZ_VERIFY_GITHUB_ORIGIN: "http://user:secret@127.0.0.1:4555" },
        { ...lab, PLOYZ_VERIFY_GITHUB_ORIGIN: "ftp://127.0.0.1:4555" },
      ];
      for (const environment of refused) {
        const failure = yield* Effect.flip(load(environment));
        assert.instanceOf(failure, InvalidConfiguration, environment.PLOYZ_VERIFY_GITHUB_ORIGIN);
      }
    }),
  );

  it.effect("points PostShiba at a verify lab's origin, outside production only", () =>
    Effect.gen(function* () {
      const real = yield* load({ ...requiredEnvironment, ...postShibaMail });
      assert.strictEqual(real.mail?.verifyOrigin, null);

      const lab = { ...requiredEnvironment, ...postShibaMail, NODE_ENV: "development" };
      const fake = yield* load({ ...lab, PLOYZ_VERIFY_POSTSHIBA_ORIGIN: "http://127.0.0.1:4556" });
      assert.strictEqual(fake.mail?.verifyOrigin, "http://127.0.0.1:4556");

      const refused = [
        { ...requiredEnvironment, ...postShibaMail, PLOYZ_VERIFY_POSTSHIBA_ORIGIN: "http://127.0.0.1:4556" },
        { ...lab, PLOYZ_VERIFY_POSTSHIBA_ORIGIN: "http://127.0.0.1:4556/api" },
      ];
      for (const environment of refused) {
        const failure = yield* Effect.flip(load(environment));
        assert.instanceOf(failure, InvalidConfiguration, environment.PLOYZ_VERIFY_POSTSHIBA_ORIGIN);
      }
    }),
  );

  it.effect("rejects malformed startup values", () =>
    Effect.gen(function* () {
      const invalid: ReadonlyArray<Record<string, string>> = [
        { APP_URL: "not-a-url" },
        { PORT: "65536" },
        { APP_ENCRYPTION_SECRET: "too-short" },
        { ...hostedPolar, POLAR_PRODUCT_ID: "not-a-uuid" },
      ];

      for (const value of invalid) {
        const failure = yield* Effect.flip(load({ ...requiredEnvironment, ...value }));
        assert.instanceOf(failure, Config.ConfigError);
      }
    }),
  );
});

describe("startupConfigFailure", () => {
  it("is undefined for a valid environment", () => {
    assert.isUndefined(startupConfigFailure(requiredEnvironment));
  });

  it("names every bad variable, without printing secret values", () => {
    const report = startupConfigFailure({
      ...without(requiredEnvironment, "GITHUB_CLIENT_ID"),
      BETTER_AUTH_SECRET: "",
      APP_URL: "cloud.example.com",
      APP_ENCRYPTION_SECRET: "short-secret-value",
    });
    assert.isString(report);
    for (const name of ["GITHUB_CLIENT_ID", "BETTER_AUTH_SECRET", "APP_URL", "APP_ENCRYPTION_SECRET"]) {
      assert.include(report, name);
    }
    assert.notInclude(report, "short-secret-value");
  });

  it("never prints a rejected URL's credentials", () => {
    const report = startupConfigFailure({
      ...requiredEnvironment,
      NODE_ENV: "development",
      PLOYZ_VERIFY_GITHUB_ORIGIN: "http://user:hunter2-password@127.0.0.1:4555",
    });
    assert.include(report, "PLOYZ_VERIFY_GITHUB_ORIGIN");
    assert.notInclude(report, "hunter2-password");
  });

  it("names a partial set's variables", () => {
    const report = startupConfigFailure({ ...requiredEnvironment, ...without(postShibaMail, "MAIL_FROM") });
    assert.include(report, "MAIL_FROM");
  });
});
