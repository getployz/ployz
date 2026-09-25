import { assert, describe, it } from "@effect/vitest";
import { Config, ConfigProvider, Effect, Redacted } from "effect";
import { AppConfig, InvalidConfiguration } from "#/server/config.server";

const requiredEnvironment = {
  DATABASE_URL: "postgres://postgres:postgres@localhost:5432/ployz_cloud",
  APP_URL: "http://localhost:3000",
  BETTER_AUTH_SECRET: "better-auth-secret",
  GITHUB_CLIENT_ID: "github-client-id",
  GITHUB_CLIENT_SECRET: "github-client-secret",
  APP_ENCRYPTION_SECRET: "app-encryption-secret-at-least-32-characters",
};

const load = (environment: Record<string, string>) =>
  AppConfig.make.pipe(
    Effect.provideService(
      ConfigProvider.ConfigProvider,
      ConfigProvider.fromEnv({ env: environment }),
    ),
  );

describe("AppConfig", () => {
  it.effect("loads startup configuration without Electric and keeps secrets redacted", () =>
    Effect.gen(function* () {
      const config = yield* load({
        ...requiredEnvironment,
      });

      assert.strictEqual(config.app.port, 3000);
      assert.strictEqual(config.ployz.installerUrl.href, "https://ployz.sh/");
      assert.strictEqual(String(config.auth.secret), "<redacted>");
      assert.strictEqual(Redacted.value(config.auth.secret), "better-auth-secret");
      assert.deepStrictEqual(config.polar, { mode: "self_hosted" });
    }),
  );

  const hostedPolar = {
    POLAR_ACCESS_TOKEN: "polar-token",
    POLAR_WEBHOOK_SECRET: "polar-webhook-secret",
    POLAR_PRODUCT_FREE_ID: "11111111-1111-4111-8111-111111111111",
    POLAR_PRODUCT_SOLO_ID: "22222222-2222-4222-8222-222222222222",
    POLAR_PRODUCT_TEAMS_ID: "33333333-3333-4333-8333-333333333333",
  };

  const { POLAR_WEBHOOK_SECRET: _webhookSecret, ...withoutWebhookSecret } = hostedPolar;
  const invalidPolar: ReadonlyArray<readonly [string, Record<string, string>, string]> = [
    ["only an access token", { POLAR_ACCESS_TOKEN: "polar-token" }, "must be entirely absent"],
    ["everything but the webhook secret", withoutWebhookSecret, "must be entirely absent"],
    ["Solo and Teams sharing a product", { ...hostedPolar, POLAR_PRODUCT_TEAMS_ID: hostedPolar.POLAR_PRODUCT_SOLO_ID }, "distinct Polar product IDs"],
  ];

  it.effect.each(invalidPolar)("rejects hosted Polar configuration with %s", ([, polar, reason]) =>
    Effect.gen(function* () {
      const failure = yield* Effect.flip(load({ ...requiredEnvironment, ...polar }));

      assert.instanceOf(failure, InvalidConfiguration);
      assert.include(failure.message, reason);
    }),
  );

  it.effect("accepts legacy Polar aliases but rejects conflicting IDs", () =>
    Effect.gen(function* () {
      const config = yield* load({
        ...requiredEnvironment,
        POLAR_ACCESS_TOKEN: "polar-token",
        POLAR_WEBHOOK_SECRET: "polar-webhook-secret",
        POLAR_PRODUCT_FREE_ID: "11111111-1111-4111-8111-111111111111",
        POLAR_PRODUCT_HOBBY_ID: "22222222-2222-4222-8222-222222222222",
        POLAR_PRODUCT_PRO_ID: "33333333-3333-4333-8333-333333333333",
      });

      assert.strictEqual(config.polar.mode, "hosted");
      if (config.polar.mode === "hosted") {
        assert.deepStrictEqual(config.polar.productIds, {
          free: "11111111-1111-4111-8111-111111111111",
          solo: "22222222-2222-4222-8222-222222222222",
          teams: "33333333-3333-4333-8333-333333333333",
        });
      }

      const failure = yield* Effect.flip(
        load({
          ...requiredEnvironment,
          POLAR_ACCESS_TOKEN: "polar-token",
          POLAR_WEBHOOK_SECRET: "polar-webhook-secret",
          POLAR_PRODUCT_FREE_ID: "11111111-1111-4111-8111-111111111111",
          POLAR_PRODUCT_SOLO_ID: "22222222-2222-4222-8222-222222222222",
          POLAR_PRODUCT_HOBBY_ID: "33333333-3333-4333-8333-333333333333",
          POLAR_PRODUCT_TEAMS_ID: "44444444-4444-4444-8444-444444444444",
        }),
      );

      assert.instanceOf(failure, InvalidConfiguration);
    }),
  );

  it.effect("rejects malformed startup values", () =>
    Effect.gen(function* () {
      const invalid: ReadonlyArray<Record<string, string>> = [
        { APP_URL: "not-a-url" },
        { PORT: "65536" },
        { PLOYZ_INSTALLER_SHA256: "not-a-digest" },
        { APP_ENCRYPTION_SECRET: "too-short" },
      ];

      for (const value of invalid) {
        const failure = yield* Effect.flip(load({ ...requiredEnvironment, ...value }));
        assert.instanceOf(failure, Config.ConfigError);
      }
    }),
  );
});
