import { loadEnvironmentDocument } from "./working-state-repository.server";
import { emptyEnvironmentIntent } from "./saved-intent";
import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { environment, member, organization, project, user } from "#/db/schema";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";
import {
  attachServiceVolume,
  updateServiceVolumeMountPath,
} from "./mount-operations.server";
import {
  createVariableGroupResource,
  createVolumeResource,
  deleteVolumeResource,
  updateEnvironmentResourceCanvasPosition,
  updateVariableGroupResource,
} from "./resource-operations.server";
import { createService } from "./service-operations.server";
import { createImageServiceSource } from "./services";

it.live(
  "keeps resource lifecycle and service-owned mounts authorized and atomic",
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
      const layer = Layer.merge(
        DatabaseLive.pipe(Layer.provide(config)),
        SecretEncryptionLive.pipe(Layer.provide(config)),
      );

      yield* Effect.gen(function* () {
        const database = yield* Database;
        const users = yield* database.drizzle
          .insert(user)
          .values([
            { email: "resources@example.test", emailVerified: true, name: "Resources" },
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
        const service = yield* createService(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          name: "API",
          source: createImageServiceSource({ image: "acme/api:latest" }),
          x: 0,
          y: 0,
          preDeployCommand: null,
          startCommand: null,
          healthcheck: { type: "none" },
          restartPolicy: "unless-stopped",
        });

        const group = yield* createVariableGroupResource(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          name: "Shared config",
          x: 10.4,
          y: 20.6,
        });
        const groupRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`
            select xmin::text as txid from environment_resource
            where id = ${group.data.resource.id}
            union all
            select xmin::text as txid from resource_lineage
            where id = ${group.data.resource.lineageId}
            union all
            select xmin::text as txid from environment_variable_group
            where id = ${group.data.variableGroup.id}
            union all
            select xmin::text as txid from variable_group_lineage
            where id = ${group.data.variableGroup.lineageId}
            union all
            select xmin::text as txid from environment_canvas_node_position
            where resource_id = ${group.data.resource.id}
          `,
          "objects",
        );
        assert.strictEqual(groupRows.length, 5);
        assert.strictEqual(new Set(groupRows.map((row) => row.txid)).size, 1);

        const volume = yield* createVolumeResource(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          name: "Data",
          x: 30.4,
          y: 40.6,
        });
        const volumeRows = yield* database.drizzle.execute<{ txid: string }>(
          sql`
            select xmin::text as txid from environment_resource
            where id = ${volume.data.resource.id}
            union all
            select xmin::text as txid from resource_lineage
            where id = ${volume.data.resource.lineageId}
            union all
            select xmin::text as txid from environment_canvas_node_position
            where resource_id = ${volume.data.resource.id}
          `,
          "objects",
        );
        assert.strictEqual(volumeRows.length, 3);
        assert.strictEqual(new Set(volumeRows.map((row) => row.txid)).size, 1);

        const renamed = yield* updateVariableGroupResource(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          resourceId: group.data.resource.id,
          name: "Runtime config",
        });
        assert.strictEqual(renamed.data.intent.variableGroups[0]?.name, "Runtime config");
        const mounted = yield* attachServiceVolume(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: service.data.service.id,
          volumeResourceId: volume.data.resource.id,
          mountPath: "/data",
        });
        assert.strictEqual(
          (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          mounted.data.revision,
        );
        const updatedMount = yield* updateServiceVolumeMountPath(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          serviceId: service.data.service.id,
          volumeResourceId: volume.data.resource.id,
          mountPath: "/var/data",
        });
        assert.strictEqual(updatedMount.data.intent.services[0]?.volumeAttachments[0]?.mountPath, "/var/data");

        yield* updateEnvironmentResourceCanvasPosition(actor, {
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          resourceId: volume.data.resource.id,
          x: 55.5,
          y: 66.6,
        });
        const movedRows = yield* database.drizzle.execute<{
          x: number;
          y: number;
          resourceType: string;
        }>(
          sql`select x, y, resource_type as "resourceType"
              from environment_canvas_node_position
              where resource_id = ${volume.data.resource.id}`,
          "objects",
        );
        assert.deepStrictEqual(movedRows, [
          { x: 56, y: 67, resourceType: "volume" },
        ]);

        const invalidTarget = yield* Effect.flip(
          attachServiceVolume(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
            organizationSlug: "acme",
            environmentId: environmentRecord.id,
            serviceId: service.data.service.id,
            volumeResourceId: group.data.resource.id,
            mountPath: "/config",
          }),
        );
        assert.strictEqual(invalidTarget._tag, "NotFound");

        const deleted = yield* deleteVolumeResource(actor, {
          revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
          organizationSlug: "acme",
          environmentId: environmentRecord.id,
          resourceId: volume.data.resource.id,
        });
        assert.deepStrictEqual(deleted.data.intent.volumes, []);
        assert.deepStrictEqual(deleted.data.intent.services[0]?.volumeAttachments, []);
        const deletedRows = yield* database.drizzle.execute<{ count: string }>(
          sql`select count(*)::text as count from environment_resource where id = ${volume.data.resource.id}`, "objects");
        assert.strictEqual(deletedRows[0]?.count, "1");

      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
