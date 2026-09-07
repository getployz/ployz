import { assert, it } from "@effect/vitest";
import { Cause, ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { Auth, AuthLive, Unauthorized } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive } from "#/server/database.server";
import { Polar } from "#/modules/billing/polar-provider.server";
import { InngestClient } from "#/modules/inngest/client";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";

it.live(
  "resolves one Better Auth session into an explicit Actor",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const provider = ConfigProvider.fromEnv({
        env: {
          NODE_ENV: "test",
          DATABASE_URL: container.url.href,
          ELECTRIC_URL: "http://localhost:30000",
          APP_URL: "http://localhost:3000",
          BETTER_AUTH_SECRET: "better-auth-secret",
          GITHUB_CLIENT_ID: "github-client-id",
          GITHUB_CLIENT_SECRET: "github-client-secret",
          PLOYZ_RELAY_URL: "https://relay.example.test",
          APP_ENCRYPTION_SECRET:
            "app-encryption-secret-at-least-32-characters",
        },
      });
      const configLayer = AppConfig.layer.pipe(
        Layer.provide(ConfigProvider.layer(provider)),
      );
      const databaseLayer = DatabaseLive.pipe(Layer.provide(configLayer));
      const layer = AuthLive.pipe(
        Layer.provide(Layer.mergeAll(
          configLayer,
          databaseLayer,
          Layer.succeed(Polar, { mode: "self_hosted" }),
          Layer.succeed(InngestClient, new Inngest({ id: "auth-test" })),
        )),
      );

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

        const anonymous = yield* Effect.exit(auth.resolveActor(new Headers()));
        assert.strictEqual(anonymous._tag, "Failure");
        if (anonymous._tag === "Failure") {
          assert.instanceOf(Cause.squash(anonymous.cause), Unauthorized);
        }
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
