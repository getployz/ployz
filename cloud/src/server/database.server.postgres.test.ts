import { assert, it } from "@effect/vitest";
import { escapeIdentifier, Pool } from "pg";
import { sql } from "drizzle-orm";
import {
  ConfigProvider,
  Data,
  Effect,
  Exit,
  Layer,
  Option,
  Stream,
} from "effect";
import { AppConfig } from "#/server/config.server";
import {
  BetterAuthDatabase,
  Database,
  DatabaseLive,
  DatabaseSubscriptionFailure,
  subscribeDatabaseNotifications,
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
          APP_URL: "http://localhost:3000",
          BETTER_AUTH_SECRET: "better-auth-secret",
          GITHUB_CLIENT_ID: "github-client-id",
          GITHUB_CLIENT_SECRET: "github-client-secret",
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
              assert.strictEqual(transaction.subscribe, database.subscribe);
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

        // Acquisition completes after LISTEN, with identifiers quoted and events buffered.
        const channel = 'database "events';
        const notifications = yield* database.subscribe(channel);
        for (const payload of ["committed", ""]) {
          yield* database.drizzle.execute(sql`select pg_notify(${channel}, ${payload})`);
          assert.deepStrictEqual(yield* Stream.runHead(notifications), Option.some(payload));
        }
        const invalidChannel = yield* Effect.flip(Effect.scoped(database.subscribe("")));
        assert.instanceOf(invalidChannel, DatabaseSubscriptionFailure);
        assert.strictEqual(invalidChannel.operation, "listen");
        assert.strictEqual(invalidChannel.channel, "");

        yield* database.drizzle.execute(sql`
          select pg_terminate_backend(pid) from pg_stat_activity
          where datname = current_database() and query = ${`LISTEN ${escapeIdentifier(channel)}`}
        `);
        const disconnected = yield* Effect.flip(Stream.runDrain(notifications));
        assert.instanceOf(disconnected, DatabaseSubscriptionFailure);
        assert.strictEqual(disconnected.operation, "receive");
        assert.strictEqual(disconnected.channel, channel);
        assert.instanceOf(disconnected.cause, Error);
      }).pipe(Effect.scoped, Effect.provide(layer));
    }),
  60_000,
);


it.live("wraps subscription connection failures in the database error channel", () =>
  Effect.gen(function* () {
    const pool = new Pool();
    yield* Effect.promise(() => pool.end());
    const failure = yield* Effect.flip(Effect.scoped(subscribeDatabaseNotifications(pool, "events")));
    assert.instanceOf(failure, DatabaseSubscriptionFailure);
    assert.strictEqual(failure.operation, "connect");
    assert.strictEqual(failure.channel, "events");
    assert.instanceOf(failure.cause, Error);
  }),
);
