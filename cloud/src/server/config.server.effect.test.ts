import { assert, describe, it } from "@effect/vitest";
import { ConfigProvider, Effect, Redacted } from "effect";
import { AppConfig } from "#/server/config.server";

const requiredEnvironment = {
  DATABASE_URL: "postgres://postgres:postgres@localhost:5432/ployz_cloud",
  ELECTRIC_URL: "http://localhost:30000",
  APP_URL: "http://localhost:3000",
  BETTER_AUTH_SECRET: "better-auth-secret",
  GITHUB_CLIENT_ID: "github-client-id",
  GITHUB_CLIENT_SECRET: "github-client-secret",
  PLOYZ_RELAY_URL: "https://relay.example.test",
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
  it.effect("loads defaults and keeps secrets redacted", () =>
    Effect.gen(function* () {
      const config = yield* load({
        ...requiredEnvironment,
        ELECTRIC_SECRET: "",
      });

      assert.strictEqual(config.app.port, 3000);
      assert.strictEqual(config.ployz.installerUrl.href, "https://ployz.sh/");
      assert.strictEqual(config.electric.secret, undefined);
      assert.strictEqual(String(config.auth.secret), "<redacted>");
      assert.strictEqual(Redacted.value(config.auth.secret), "better-auth-secret");
      assert.deepStrictEqual(config.polar, { mode: "self_hosted" });
    }),
  );

  it.effect("requires the complete hosted Polar configuration", () =>
    Effect.gen(function* () {
      const exit = yield* Effect.exit(
        load({
          ...requiredEnvironment,
          POLAR_ACCESS_TOKEN: "polar-token",
        }),
      );

      assert.isTrue(exit._tag === "Failure");
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

      const exit = yield* Effect.exit(
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

      assert.isTrue(exit._tag === "Failure");
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
        const exit = yield* Effect.exit(load({ ...requiredEnvironment, ...value }));
        assert.isTrue(exit._tag === "Failure");
      }
    }),
  );
});
