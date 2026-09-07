import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import {
  ConfigProvider,
  Data,
  Effect,
  Exit,
  Layer,
} from "effect";
import { AppConfig } from "#/server/config.server";
import {
  BetterAuthDatabase,
  Database,
  DatabaseLive,
} from "#/server/database.server";
import { postgresTestContainer } from "#/test/postgres";

class Rollback extends Data.TaggedError("Rollback") {}

const nestedInsert = Effect.fn("DatabaseTest.nestedInsert")(function* () {
  const database = yield* Database;
  yield* database.drizzle.execute(
    sql`insert into effect_transaction_test (value) values ('nested')`,
  );
});

it.live(
  "shares one pool with Better Auth and keeps nested operations in the transaction",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      const provider = ConfigProvider.fromEnv({
        env: {
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
      const layer = DatabaseLive.pipe(Layer.provide(configLayer));

      yield* Effect.gen(function* () {
        const database = yield* Database;
        const betterAuthDatabase = yield* BetterAuthDatabase;

        yield* Effect.promise(() =>
          betterAuthDatabase.drizzle.execute(sql`
            create table effect_transaction_test (
              id integer generated always as identity primary key,
              value text not null
            )
          `),
        );

        const result = yield* Effect.exit(
          database.transaction(
            Effect.gen(function* () {
              const transaction = yield* Database;
              yield* transaction.transaction(nestedInsert());
              return yield* new Rollback();
            }),
          ),
        );
        assert.deepStrictEqual(result, Exit.fail(new Rollback()));

        const count = yield* Effect.promise(() =>
          betterAuthDatabase.drizzle.execute<{ count: number }>(
            sql`select count(*)::integer as count from effect_transaction_test`,
          ),
        );
        assert.strictEqual(count.rows[0]?.count, 0);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
