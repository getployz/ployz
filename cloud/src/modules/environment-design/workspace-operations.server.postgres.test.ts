import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { member, organization, user } from "#/db/schema";
import { Database, DatabaseLive } from "#/server/database.server";
import { AppConfig } from "#/server/config.server";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";
import {
  createEmptyProject,
  listProjects,
} from "./workspace-operations.server";

it.live(
  "authorizes workspace writes and returns the PostgreSQL transaction that committed every created row",
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
                PLOYZ_RELAY_URL: "https://relay.example.test",
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
        const users = yield* database.drizzle
          .insert(user)
          .values([
            {
              email: "author@example.test",
              emailVerified: true,
              name: "Author",
            },
            {
              email: "stranger@example.test",
              emailVerified: true,
              name: "Stranger",
            },
          ])
          .returning({ id: user.id });
        const author = users[0];
        const stranger = users[1];
        if (author === undefined || stranger === undefined) {
          return yield* Effect.die("PostgreSQL did not return the test users.");
        }
        const organizations = yield* database.drizzle
          .insert(organization)
          .values({ name: "Acme", slug: "acme" })
          .returning({ id: organization.id });
        const organizationRecord = organizations[0];
        if (organizationRecord === undefined) {
          return yield* Effect.die("PostgreSQL did not return the test organization.");
        }
        yield* database.drizzle.insert(member).values({
          userId: author.id,
          organizationId: organizationRecord.id,
          role: "owner",
        });

        const receipt = yield* createEmptyProject(
          { userId: author.id },
          { organizationSlug: "acme" },
        );

        const committedRows = yield* database.drizzle.execute<{
          txid: string;
        }>(sql`
          select xmin::text as txid from project where id = ${receipt.data.project.id}
          union all
          select xmin::text as txid from environment where id = ${receipt.data.environment.id}
          union all
          select xmin::text as txid from user_project_preference
          where user_id = ${author.id} and project_id = ${receipt.data.project.id}
        `, "objects");
        assert.strictEqual(committedRows.length, 3);
        assert.strictEqual(new Set(committedRows.map((row) => row.txid)).size, 1);

        const unauthorized = yield* Effect.flip(
          listProjects(
            { userId: stranger.id },
            { organizationSlug: "acme" },
          ),
        );
        assert.strictEqual(unauthorized._tag, "NotFound");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
