import { assert, describe, it } from "@effect/vitest";
import { Config, ConfigProvider, Effect, Redacted } from "effect";
import { AppConfig, InvalidConfiguration } from "#/server/config.server";

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
    }),
  );

  it.effect("names each missing required variable", () =>
    Effect.gen(function* () {
      for (const name of Object.keys(requiredEnvironment)) {
        const failure = yield* Effect.flip(load(without(requiredEnvironment, name)));
        assert.instanceOf(failure, Config.ConfigError);
        assert.include(String(failure), name);
      }
    }),
  );

  it.effect("names every missing required variable at once", () =>
    Effect.gen(function* () {
      const failure = yield* Effect.flip(load({}));
      assert.instanceOf(failure, Config.ConfigError);
      for (const name of Object.keys(requiredEnvironment)) {
        assert.include(failure.message, name);
      }
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
