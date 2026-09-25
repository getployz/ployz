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
