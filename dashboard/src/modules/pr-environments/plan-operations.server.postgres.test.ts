import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { eq, sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { environment, member, organization, prEnvironmentPlan, user } from "#/db/schema";
import { Database, DatabaseLive } from "#/server/database.server";
import { AppConfig } from "#/server/config.server";
import { postgresTestDatabase } from "#/test/postgres";
import { createEmptyProject, createEnvironment } from "#/modules/environment-design/workspace-operations.server";
import { createGitServiceSource } from "#/modules/environment-design/services";
import { setPrEnvironmentPlan } from "./plan-operations.server";

it.live("saves a repository's PR Environments plan, refuses another project's start-from, and asks again after a teardown", () =>
  Effect.gen(function* () {
    const testDatabase = yield* postgresTestDatabase;
    const config = AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({
      env: { ...testConfigEnvironment(), DATABASE_URL: testDatabase.url.href },
    }))));

    yield* Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [author, other] = yield* drizzle.insert(user).values([
        { email: "author@example.test", emailVerified: true, name: "Author" },
        { email: "other@example.test", emailVerified: true, name: "Other" },
      ]).returning({ id: user.id });
      const [acme] = yield* drizzle.insert(organization).values({ name: "Acme", slug: "acme" }).returning({ id: organization.id });
      if (!author || !other || !acme) return yield* Effect.die("PostgreSQL did not return the test rows.");
      yield* drizzle.insert(member).values([
        { userId: author.id, organizationId: acme.id, role: "owner" },
        { userId: other.id, organizationId: acme.id, role: "member" },
      ]);
      const actor = { userId: author.id };
      const created = yield* createEmptyProject(actor, { organizationSlug: "acme" });
      const projectSlug = created.data.project.slug;
      const production = created.data.environment;
      const staging = (yield* createEnvironment(actor, { organizationSlug: "acme", projectSlug, name: "Staging" })).data;
      const elsewhere = (yield* createEmptyProject(actor, { organizationSlug: "acme" })).data.environment;
      // Production's Working State deploys acme/app through the GitHub App.
      const source = createGitServiceSource({ repository: "acme/app", repositoryId: 42, access: { type: "github-installation", installationId: 7 } });
      yield* drizzle.update(environment)
        .set({ intent: sql`jsonb_set(${environment.intent}, '{services}', ${JSON.stringify([{ config: { source } }])}::jsonb)` })
        .where(eq(environment.id, production.id));

      const plan = {
        organizationSlug: "acme", projectSlug, repositoryId: 42, enabled: true, startFromEnvironmentId: staging.id,
        picks: { preset: "uses" as const }, setupCommands: [], removeOnClose: false, includeBots: true,
      };
      const saved = yield* setPrEnvironmentPlan(actor, plan);
      assert.deepStrictEqual(
        { ...saved, updatedAt: null },
        {
          organizationId: acme.id, projectId: created.data.project.id, repositoryId: 42, installationId: 7, repository: "acme/app",
          enabled: true, startFromEnvironmentId: staging.id, picks: { preset: "uses" }, setupCommands: [], removeOnClose: false,
          includeBots: true, enabledByUserId: author.id, updatedAt: null,
        },
      );

      // Another member's edit keeps who turned it on; turning it off clears them.
      const edited = yield* setPrEnvironmentPlan({ userId: other.id }, { ...plan, includeBots: false });
      assert.strictEqual(edited.enabledByUserId, author.id);
      assert.strictEqual((yield* setPrEnvironmentPlan({ userId: other.id }, { ...plan, enabled: false })).enabledByUserId, null);
      assert.strictEqual((yield* setPrEnvironmentPlan({ userId: other.id }, plan)).enabledByUserId, other.id);

      const refusals = [
        yield* Effect.flip(setPrEnvironmentPlan(actor, { ...plan, startFromEnvironmentId: elsewhere.id })),
        yield* Effect.flip(setPrEnvironmentPlan(actor, { ...plan, repositoryId: 99 })),
        yield* Effect.flip(setPrEnvironmentPlan({ userId: author.id }, { ...plan, organizationSlug: "nope" })),
      ];
      assert.deepStrictEqual(refusals.map((error) => error._tag), ["Validation", "Validation", "NotFound"]);

      // A torn-down start-from leaves the plan asking for another.
      yield* drizzle.delete(environment).where(eq(environment.id, staging.id));
      const [asking] = yield* drizzle.select().from(prEnvironmentPlan);
      assert.strictEqual(asking?.startFromEnvironmentId, null);
      assert.strictEqual(asking?.enabled, true);
    }).pipe(Effect.provide(DatabaseLive.pipe(Layer.provide(config))));
  }));
