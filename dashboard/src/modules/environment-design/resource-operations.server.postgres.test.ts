import { testConfigEnvironment } from "#/test/config-environment";
import { saveReviewedEnvironmentState } from "./saved-state-operations.server";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "./working-state-fingerprint.server";
import { loadCurrentEnvironmentState, loadEnvironmentDocument } from "./working-state-repository.server";
import { emptyEnvironmentIntent } from "./saved-intent";
import { assert, it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import { environment, member, organization, project, user, environmentSavedStateSnapshot } from "#/db/schema";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";
import {
  postgresTestDatabase,
} from "#/test/postgres";
import {
  attachServiceVolume,
  updateServiceVolumeMountPath,
} from "./mount-operations.server";
import {
  createVolumeResource,
  deleteVolumeResource,
  updateEnvironmentResourceCanvasPosition,
} from "./resource-operations.server";
import { createService } from "./service-operations.server";
import { createImageServiceSource } from "./services";

it.live(
  "keeps resource lifecycle and service-owned mounts authorized and atomic",
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
            volumeResourceId: service.data.service.id,
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
        assert.strictEqual(deletedRows[0]?.count, "0");
        const metadata = yield* database.drizzle.execute<{ count: string }>(sql`
          select count(*)::text as count from environment_node_introduction where node_id = ${volume.data.resource.id}
          union all select count(*)::text from environment_canvas_node_position where resource_id = ${volume.data.resource.id}
        `, "objects");
        assert.deepStrictEqual(metadata.map((row) => row.count), ["0", "0"]);
        for (let attempt = 0; attempt < 3; attempt++) {
          const racing = yield* createVolumeResource(actor, {
            organizationSlug: "acme", environmentId: environmentRecord.id,
            name: `Concurrent ${attempt}`, x: 0, y: 0,
          });
          const input = { organizationSlug: "acme", environmentId: environmentRecord.id, resourceId: racing.data.resource.id };
          const revision = (yield* loadEnvironmentDocument(environmentRecord.id)).revision;
          yield* Effect.all([
            deleteVolumeResource(actor, { ...input, revision }),
            updateEnvironmentResourceCanvasPosition(actor, { ...input, x: 42, y: 42 }).pipe(
              Effect.catchTag("NotFound", () => Effect.succeed(null)),
            ),
          ], { concurrency: "unbounded" });
          const positions = yield* database.drizzle.execute<{ count: string }>(
            sql`select count(*)::text as count from environment_canvas_node_position where resource_id = ${input.resourceId}`, "objects");
          assert.strictEqual(positions[0]?.count, "0");
        }

        const published = yield* createVolumeResource(actor, {
          organizationSlug: "acme", environmentId: environmentRecord.id,
          name: "Concurrent publication", x: 0, y: 0,
        });
        const reviewed = yield* loadCurrentEnvironmentState(environmentRecord.id);
        yield* Effect.all([
          saveReviewedEnvironmentState({
            environmentId: environmentRecord.id, actorId: author.id, message: null,
            review: {
              savedStateBasis: { kind: "no_saved_state" },
              workingStateFingerprint: fingerprintReviewedEnvironmentWorkingStateSync(reviewed.projection),
              destructiveServiceIds: [], destructiveVolumeReviews: [],
            },
          }).pipe(Effect.catchTag("Conflict", () => Effect.succeed(null))),
          deleteVolumeResource(actor, {
            organizationSlug: "acme", environmentId: environmentRecord.id,
            resourceId: published.data.resource.id, revision: reviewed.document.revision,
          }),
        ], { concurrency: "unbounded" });
        const dangling = yield* database.drizzle.execute<{ count: string }>(sql`
          select count(*)::text as count from environment_saved_state_snapshot saved,
            jsonb_array_elements(saved.intent->'volumes') volume
          where saved.environment_id = ${environmentRecord.id}
            and not exists (select 1 from environment_resource resource where resource.id::text = volume->>'resourceId')
        `, "objects");
        assert.strictEqual(dangling[0]?.count, "0");

        // Saved history and other node introductions independently retain identity.
        for (const retainedBy of ["saved", "introduction"] as const) {
          const retained = yield* createVolumeResource(actor, {
            organizationSlug: "acme", environmentId: environmentRecord.id,
            name: `Retained ${retainedBy}`, x: 0, y: 0,
          });
          if (retainedBy === "saved") {
            yield* database.drizzle.insert(environmentSavedStateSnapshot).values({
              organizationId: organizationRecord.id, environmentId: environmentRecord.id,
              actorId: author.id, intent: (yield* loadEnvironmentDocument(environmentRecord.id)).intent,
              volumeDeletionAuthorizations: [],
            });
          } else {
            yield* createVolumeResource(actor, {
              organizationSlug: "acme", environmentId: environmentRecord.id,
              name: "Retained introduction", x: 0, y: 0,
            });
          }
          yield* deleteVolumeResource(actor, {
            revision: (yield* loadEnvironmentDocument(environmentRecord.id)).revision,
            organizationSlug: "acme", environmentId: environmentRecord.id, resourceId: retained.data.resource.id,
          });
          const rows = yield* database.drizzle.execute<{ count: string }>(
            sql`select count(*)::text as count from environment_resource where id = ${retained.data.resource.id}`, "objects");
          assert.strictEqual(rows[0]?.count, "1", retainedBy);
        }


      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
