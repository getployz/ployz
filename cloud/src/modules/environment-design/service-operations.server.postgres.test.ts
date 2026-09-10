import { loadEnvironmentDocument, loadCurrentEnvironmentState } from "./working-state-repository.server";
import { emptyEnvironmentIntent } from "./saved-intent";
import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { Polar } from "#/modules/billing/polar-provider.server";
import {
  environment,
  member,
  organization,
  project,
  user,
} from "#/db/schema";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";
import {
  clearServiceRegistryCredential,
  createService,
  restoreServiceRegistryCredential,
  setServiceRegistryCredential,
  updateService,
  updateServiceCanvasPosition,
} from "./service-operations.server";
import { createImageServiceSource } from "./services";

it.live(
  "keeps service, credential, and canvas authoring authorized and atomic",
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
      const layer = Layer.merge(
        DatabaseLive.pipe(Layer.provide(config)),
        Layer.succeed(Polar, { mode: "self_hosted" }),
      ).pipe(
        Layer.merge(SecretEncryptionLive.pipe(Layer.provide(config))),
      );

      yield* Effect.gen(function* () {
        const database = yield* Database;
        const users = yield* database.drizzle
          .insert(user)
          .values([
            { email: "service@example.test", emailVerified: true, name: "Service" },
            { email: "outsider@example.test", emailVerified: true, name: "Outsider" },
          ])
          .returning({ id: user.id });
        const author = users[0];
        const outsider = users[1];
        if (author === undefined || outsider === undefined) {
          return yield* Effect.die("PostgreSQL did not return the test users.");
        }
        const organizations = yield* database.drizzle
          .insert(organization)
          .values({ name: "Acme", slug: "acme" })
          .returning({ id: organization.id });
        const organizationRecord = organizations[0];
        if (organizationRecord === undefined) {
          return yield* Effect.die("PostgreSQL did not return the organization.");
        }
        yield* database.drizzle.insert(member).values({
          userId: author.id,
          organizationId: organizationRecord.id,
          role: "owner",
        });
        const projects = yield* database.drizzle
          .insert(project)
          .values({ organizationId: organizationRecord.id, name: "API", slug: "api" })
          .returning({ id: project.id });
        const projectRecord = projects[0];
        if (projectRecord === undefined) {
          return yield* Effect.die("PostgreSQL did not return the project.");
        }
        const environments = yield* database.drizzle
          .insert(environment)
          .values({
            organizationId: organizationRecord.id,
            projectId: projectRecord.id,
            name: "Production",
            namespace: "api-production",
            intent: emptyEnvironmentIntent("api-production"),
          })
          .returning({ id: environment.id });
        const environmentRecord = environments[0];
        if (environmentRecord === undefined) {
          return yield* Effect.die("PostgreSQL did not return the environment.");
        }
        const actor = { userId: author.id };

        const created = yield* createService(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          name: "Registry",
          source: createImageServiceSource({ image: "acme/api:latest" }),
          x: 10.4,
          y: 20.6,
          preDeployCommand: null,
          startCommand: null,
          healthcheck: { type: "none" },
          restartPolicy: "unless-stopped",
        });
        const committed = yield* database.drizzle.execute<{ txid: string }>(
          sql`
            select xmin::text as txid from service
            where id = ${created.data.service.id}
            union all
            select xmin::text as txid from environment_canvas_node_position
            where resource_id = ${created.data.service.id}
          `,
          "objects",
        );
        assert.strictEqual(committed.length, 2);
        assert.strictEqual(new Set(committed.map((row) => row.txid)).size, 1);

        const updated = yield* updateService(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
          name: "Registry API",
          managedHostname: { prefix: "public-api", targetPort: 4000 },
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
        });
        assert.strictEqual(updated.data.intent.services[0]?.config.name, "Registry API");
        assert.deepStrictEqual((yield* loadCurrentEnvironmentState(environmentRecord.id)).intent.services[0]?.config.managedHostname, { prefix: "public-api", targetPort: 4000 });

        yield* updateServiceCanvasPosition(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
          x: 30.2,
          y: 40.8,
        });
        yield* setServiceRegistryCredential(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
          username: "octocat",
          secret: "secret-token",
        });
        yield* clearServiceRegistryCredential(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
        });
        yield* restoreServiceRegistryCredential(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: created.data.service.id,
        });

      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
