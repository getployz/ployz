import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { eq, sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { environment, member, organization, project, user } from "#/db/schema";
import { Database, DatabaseLive } from "#/server/database.server";
import { AppConfig } from "#/server/config.server";
import {
  postgresTestDatabase,
} from "#/test/postgres";
import {
  createEmptyProject,
  createEnvironment,
  setProjectDefaultEnvironment,
} from "./workspace-operations.server";
import { resolveDefaultEnvironment } from "./workspace.queries";

it.live(
  "authorizes workspace writes and returns the PostgreSQL transaction that committed every created row",
  () =>
    Effect.gen(function* () {
      const testDatabase = yield* postgresTestDatabase;
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                ...testConfigEnvironment(),
                DATABASE_URL: testDatabase.url.href,
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
        `, "objects");
        assert.strictEqual(committedRows.length, 2);
        assert.strictEqual(new Set(committedRows.map((row) => row.txid)).size, 1);
        assert.strictEqual(receipt.data.project.defaultEnvironmentId, receipt.data.environment.id);

        const staging = yield* createEnvironment(
          { userId: author.id },
          { organizationSlug: "acme", projectSlug: receipt.data.project.slug, name: "Staging" },
        );
        const projectSlug = receipt.data.project.slug;
        const chosen = yield* setProjectDefaultEnvironment(
          { userId: author.id },
          { organizationSlug: "acme", projectSlug, environmentId: staging.data.id },
        );
        assert.strictEqual(chosen.defaultEnvironmentId, staging.data.id);

        const other = yield* createEmptyProject({ userId: author.id }, { organizationSlug: "acme" });
        const refused = yield* Effect.flip(setProjectDefaultEnvironment(
          { userId: author.id },
          { organizationSlug: "acme", projectSlug, environmentId: other.data.environment.id },
        ));
        assert.strictEqual(refused._tag, "NotFound");
        const [unchanged] = yield* database.drizzle.select().from(project).where(eq(project.id, receipt.data.project.id));
        assert.strictEqual(unchanged?.defaultEnvironmentId, staging.data.id);

        // Teardown deletes the Environment row; the project then falls back to its oldest Environment.
        yield* database.drizzle.delete(environment).where(eq(environment.id, staging.data.id));
        const [tornDown] = yield* database.drizzle.select().from(project).where(eq(project.id, receipt.data.project.id));
        assert.strictEqual(tornDown?.defaultEnvironmentId, null);
        const remaining = yield* database.drizzle.select().from(environment).where(eq(environment.projectId, receipt.data.project.id));
        assert.strictEqual(tornDown && resolveDefaultEnvironment(tornDown, remaining)?.id, receipt.data.environment.id);

        const unauthorized = yield* Effect.flip(
          createEmptyProject(
            { userId: stranger.id },
            { organizationSlug: "acme" },
          ),
        );
        assert.strictEqual(unauthorized._tag, "NotFound");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
