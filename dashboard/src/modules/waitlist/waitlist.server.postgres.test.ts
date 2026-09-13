import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { waitlist } from "#/db/schema";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";
import { handleWaitlistRequest } from "./waitlist.server";

function request(body: string) {
  return new Request("http://localhost/api/waitlist", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
  });
}

const json = (response: Response) =>
  Effect.promise(() => response.json() as Promise<unknown>);

it.live(
  "canonicalizes enrollment, keeps duplicates private, and redacts failures",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                DATABASE_URL: container.url.href,
                APP_URL: "http://localhost:3000",
                BETTER_AUTH_SECRET: "better-auth-secret",
                GITHUB_CLIENT_ID: "github-client-id",
                GITHUB_CLIENT_SECRET: "github-client-secret",
                APP_ENCRYPTION_SECRET:
                  "app-encryption-secret-at-least-32-characters",
              },
            }),
          ),
        ),
      );
      const layer = DatabaseLive.pipe(Layer.provide(config));

      yield* Effect.gen(function* () {
        const database = yield* Database;

        const created = yield* handleWaitlistRequest(
          request(JSON.stringify({ email: "Person@Example.COM" })),
        );
        assert.strictEqual(created.status, 200);
        assert.deepStrictEqual(yield* json(created), { ok: true });

        const stored = yield* database.drizzle
          .select({ email: waitlist.email })
          .from(waitlist);
        assert.deepStrictEqual(stored, [{ email: "person@example.com" }]);

        const duplicate = yield* handleWaitlistRequest(
          request(JSON.stringify({ email: "person@example.com" })),
        );
        assert.strictEqual(duplicate.status, 200);
        assert.deepStrictEqual(yield* json(duplicate), { ok: true });

        const counts = yield* database.drizzle.execute<{ count: number }>(
          sql`select count(*)::integer as count from waitlist`,
          "objects",
        );
        assert.deepStrictEqual(counts, [{ count: 1 }]);

        for (const body of [
          JSON.stringify({ email: " person@example.com " }),
          JSON.stringify({ email: `${"a".repeat(243)}@example.test` }),
          JSON.stringify({ email: "not-an-email" }),
          "{",
        ]) {
          const invalid = yield* handleWaitlistRequest(request(body));
          assert.strictEqual(invalid.status, 400);
          assert.deepStrictEqual(yield* json(invalid), { ok: false });
        }

        yield* database.drizzle.execute(sql`drop table waitlist`);
        const failed = yield* handleWaitlistRequest(
          request(JSON.stringify({ email: "private@example.com" })),
        );
        assert.strictEqual(failed.status, 500);
        assert.deepStrictEqual(yield* json(failed), { ok: false });
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
