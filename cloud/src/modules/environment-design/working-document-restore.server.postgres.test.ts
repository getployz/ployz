import { fingerprintReviewedEnvironmentWorkingState, projectReviewedEnvironmentWorkingState } from "./working-state-review";
import { restoreWorkingDocument } from "./working-document-restore.server";
import { discardEnvironmentChanges } from "./environment-change-set.server";
import type { DiscardEnvironmentChangesInput } from "./working-document-restore";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { loadLatestEnvironmentSavedState } from "./saved-state-repository.server";
import { createVariableGroupResource, createVolumeResource, deleteVolumeResource } from "./resource-operations.server";
import { attachServiceVolume } from "./mount-operations.server";
import { attachServiceVariableGroup, createServiceVariable, updateServiceVariable } from "./variable-operations.server";
import { loadEnvironmentNodeIntroductionIntent } from "./environment-node-introduction.repository.server";
import { environmentNodeIntroductionSchema } from "./environment-node-introductions";
import { environmentNodeConfigSnapshot, environmentNodeIntroduction } from "#/modules/runtime/tables";
import { decodeStrict } from "./schema";
import { loadCurrentEnvironmentState, writeEnvironmentDocument } from "./working-state-repository.server";
import { environmentDeployment, environmentSavedStateSnapshot } from "#/modules/deployments/tables";
import { withMutationResult } from "#/server/mutation-result.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { canonicalizeEnvironmentIntent, compileEnvironmentIntent } from "@ployz/sdk/config";
import { loadEnvironmentDocument } from "./working-state-repository.server";
import { emptyEnvironmentIntent } from "./saved-intent";
import { assert, it } from "@effect/vitest";
import { eq, sql } from "drizzle-orm";
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
  createService,
  setServiceRegistryCredential,
  updateService,
} from "./service-operations.server";
import { createImageServiceSource } from "./services";

it.live(
  "restores one authorized document revision with captured secrets and retained identities",
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
        const scope = { organizationSlug: "acme", environmentId: environmentRecord.id };
        const serviceId = created.data.service.id;
        const revision = () => loadEnvironmentDocument(environmentRecord.id).pipe(Effect.map((document) => document.revision));
        const volume = yield* createVolumeResource(actor, { ...scope, name: "Data", x: 1, y: 2 });
        const group = yield* createVariableGroupResource(actor, { ...scope, name: "Shared", x: 3, y: 4 });
        const introductionIdentity = { environmentId: environmentRecord.id, nodeType: "service" as const, nodeId: serviceId };
        const introduced = yield* loadEnvironmentNodeIntroductionIntent(introductionIdentity);
        const publicIntroductions = yield* database.drizzle.select().from(environmentNodeIntroduction)
          .where(eq(environmentNodeIntroduction.environmentId, environmentRecord.id));
        assert.deepStrictEqual(publicIntroductions.map((row) => decodeStrict(environmentNodeIntroductionSchema, row).nodeType).sort(),
          ["service", "variable_group", "volume"]);
        assert.ok(!JSON.stringify(publicIntroductions).includes("ciphertext"));
        yield* attachServiceVariableGroup(actor, { ...scope, revision: yield* revision(), serviceId,
          variableGroupId: group.data.variableGroup.id });
        yield* setServiceRegistryCredential(actor, { ...scope, revision: yield* revision(), serviceId,
          username: "temporary-owner", secret: "temporary-credential" });
        for (const path of ["variableGroupAttachments", "source.credentials"]) {
          yield* discardEnvironmentChanges(actor, { ...scope, revision: yield* revision(),
            savedStateBasis: { kind: "no_saved_state" }, baselineToken: "applied:none",
            command: { kind: "node", nodeType: "service", nodeId: serviceId, path } });
        }
        const restoredIntroduction = (yield* loadEnvironmentDocument(environmentRecord.id)).intent.services[0];
        assert.deepStrictEqual(restoredIntroduction?.variableGroupAttachments, []);
        assert.deepStrictEqual(restoredIntroduction?.config.source, introduced.services[0]?.config.source);
        assert.deepStrictEqual(yield* loadEnvironmentNodeIntroductionIntent(introductionIdentity), introduced);
        assert.strictEqual(yield* loadLatestEnvironmentSavedState(scope.environmentId), null);
        yield* attachServiceVolume(actor, { ...scope, revision: yield* revision(), serviceId,
          volumeResourceId: volume.data.resource.id, mountPath: "/data" });
        const sealed = yield* createServiceVariable(actor, { ...scope, revision: yield* revision(), serviceId,
          key: "TOKEN", description: "secret", exported: true, value: { type: "sealed", value: "original-secret" } });
        const variableId = sealed.data.intent.services[0]?.variables[0]?.id;
        if (!variableId) return yield* Effect.die("Variable missing.");
        yield* setServiceRegistryCredential(actor, { ...scope, revision: yield* revision(), serviceId, username: "owner", secret: "original-registry-secret" });
        const baseline = (yield* loadCurrentEnvironmentState(environmentRecord.id)).intent;
        const [saved] = yield* database.drizzle.insert(environmentSavedStateSnapshot).values({
          organizationId: organizationRecord.id, environmentId: environmentRecord.id, actorId: actor.userId,
          intent: baseline, volumeDeletionAuthorizations: [],
        }).returning({ id: environmentSavedStateSnapshot.id });
        if (!saved) return yield* Effect.die("Saved intent missing.");
        const snapshotSource = { kind: "saved" as const, environmentSavedStateSnapshotId: saved.id };
        const oldRevision = yield* revision();
        yield* updateService(actor, { ...scope, revision: oldRevision, serviceId, name: "Changed API", replicas: 3 });
        yield* updateServiceVariable(actor, { ...scope, revision: yield* revision(), serviceId, variableId,
          key: "TOKEN", description: null, exported: false, value: { type: "sealed", value: "new-secret" } });
        yield* setServiceRegistryCredential(actor, { ...scope, revision: yield* revision(), serviceId, username: "new-owner", secret: "new-registry-secret" });
        yield* restoreWorkingDocument(actor, { ...scope, revision: yield* revision(), snapshotSource,
          command: { kind: "node", nodeType: "service", nodeId: serviceId, path: "source.credentials" } });
        const restoredCredential = (yield* loadCurrentEnvironmentState(environmentRecord.id)).intent.services[0];
        const credentialEncryption = yield* SecretEncryption;
        if (!restoredCredential?.encryptedRegistrySecret) return yield* Effect.die("Restored credential missing.");
        assert.strictEqual(credentialEncryption.decrypt(restoredCredential.encryptedRegistrySecret), "original-registry-secret");
        assert.deepStrictEqual(restoredCredential.config.source, baseline.services[0]?.config.source);
        assert.strictEqual(restoredCredential.config.name, "Changed API");
        yield* deleteVolumeResource(actor, { ...scope, revision: yield* revision(), resourceId: volume.data.resource.id });
        const current = yield* loadEnvironmentDocument(environmentRecord.id);
        const command = { kind: "all" as const };
        const stale = yield* Effect.flip(restoreWorkingDocument(actor, { ...scope, revision: oldRevision, snapshotSource, command }));
        assert.strictEqual(stale._tag, "Conflict");
        const unauthorized = yield* Effect.flip(restoreWorkingDocument({ userId: outsider.id }, { ...scope, revision: current.revision, snapshotSource, command }));
        assert.strictEqual(unauthorized._tag, "NotFound");
        assert.strictEqual((yield* revision()), current.revision);
        const restored = yield* restoreWorkingDocument(actor, { ...scope, revision: current.revision, snapshotSource, command });
        assert.notStrictEqual(restored.data.revision, current.revision);
        assert.deepStrictEqual((yield* loadCurrentEnvironmentState(environmentRecord.id)).intent, canonicalizeEnvironmentIntent(baseline));
        assert.ok(!JSON.stringify(restored.data.intent).includes("ciphertext"));
        const renderedReview = projectReviewedEnvironmentWorkingState({ ...restored.data,
          compiled: compileEnvironmentIntent(environmentRecord.id, restored.data.intent) });
        const captured = yield* loadCurrentEnvironmentState(environmentRecord.id);
        assert.strictEqual(yield* Effect.promise(() => fingerprintReviewedEnvironmentWorkingState(renderedReview)),
          yield* Effect.promise(() => fingerprintReviewedEnvironmentWorkingState(captured.projection)));

        const encryption = yield* SecretEncryption;
        const restoredSecret = (yield* loadCurrentEnvironmentState(environmentRecord.id)).intent.services[0]?.variables[0]?.value;
        if (restoredSecret?.kind !== "secret" || !restoredSecret.encryptedValue) return yield* Effect.die("Captured secret missing.");
        assert.strictEqual(encryption.decrypt(restoredSecret.encryptedValue), "original-secret");
        const written = yield* database.drizzle.execute<{ txid: string }>(sql`
          select xmin::text as txid from environment where id = ${environmentRecord.id}
          union all select xmin::text as txid from variable_secret where variable_id = ${variableId}
          union all select xmin::text as txid from service_registry_credential where service_id = ${serviceId}`, "objects");
        assert.strictEqual(written.length, 3);
        assert.strictEqual(new Set(written.map((row) => row.txid)).size, 1);
        // Identity validation is inside the single write boundary, including restores.
        const candidate = structuredClone(restored.data.intent);
        const first = candidate.services[0];
        if (!first) return yield* Effect.die("Service missing.");
        first.lineageId = volume.data.resource.lineageId;
        const identityError = yield* Effect.flip(withMutationResult(writeEnvironmentDocument(restored.data, candidate)));
        assert.strictEqual(identityError._tag, "Conflict");
        assert.strictEqual((yield* revision()), restored.data.revision);

        const recordAttempt = (replicas: number, status: "applied" | "queued") => Effect.gen(function* () {
          const intent = structuredClone(baseline);
          const service = intent.services[0];
          if (!service) return yield* Effect.die("Service missing.");
          service.config.replicas = replicas;
          const [saved] = yield* database.drizzle.insert(environmentSavedStateSnapshot).values({
            organizationId: organizationRecord.id, environmentId: scope.environmentId, actorId: actor.userId,
            intent, volumeDeletionAuthorizations: [],
          }).returning();
          if (!saved) return yield* Effect.die("Saved revision missing.");
          const [attempt] = yield* database.drizzle.insert(environmentDeployment).values({
            organizationId: organizationRecord.id, environmentId: scope.environmentId,
            savedStateSnapshotId: saved.id, status, triggerOrigin: { origin: "manual", actorId: actor.userId },
          }).returning();
          if (!attempt) return yield* Effect.die("Attempt missing.");
          for (const node of compileEnvironmentIntent(scope.environmentId, intent).nodeSnapshots) {
            yield* database.drizzle.insert(environmentNodeConfigSnapshot).values({
              ...node, organizationId: organizationRecord.id, environmentId: scope.environmentId,
              environmentDeploymentId: attempt.id,
            });
          }
          return attempt;
        });
        const reviewDiscard = (command: DiscardEnvironmentChangesInput["command"]) => Effect.gen(function* () {
          const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: scope.environmentId });
          const state = projection.explicitStates[0];
          if (!state?.saved) return yield* Effect.die("Saved projection missing.");
          return { ...scope, revision: yield* revision(), command,
            baselineToken: (state.deploymentEvidence ?? state.applied).token,
            savedStateBasis: { kind: "saved_revision" as const, savedStateSnapshotId: state.saved.snapshotId },
          };
        });
        yield* recordAttempt(1, "applied");
        const active = yield* recordAttempt(5, "queued");
        yield* updateService(actor, { ...scope, revision: yield* revision(), serviceId, replicas: 7, name: "Later edit" });
        const field = { kind: "node" as const, nodeType: "service" as const, nodeId: serviceId, path: "replicas" };
        const reviewed = yield* reviewDiscard(field);
        const denied = yield* Effect.flip(discardEnvironmentChanges({ userId: outsider.id }, reviewed));
        assert.strictEqual(denied._tag, "NotFound");
        yield* discardEnvironmentChanges(actor, reviewed);
        assert.strictEqual((yield* loadEnvironmentDocument(scope.environmentId)).intent.services[0]?.config.replicas, 5);
        assert.strictEqual((yield* loadLatestEnvironmentSavedState(scope.environmentId))?.intent.services[0]?.config.replicas, 5);
        assert.strictEqual((yield* loadEnvironmentDocument(scope.environmentId)).intent.services[0]?.config.name, "Later edit");

        yield* updateService(actor, { ...scope, revision: yield* revision(), serviceId, replicas: 7 });
        const staleDiscard = yield* Effect.flip(discardEnvironmentChanges(actor, reviewed));
        assert.strictEqual(staleDiscard._tag, "Conflict");
        const beforeFailure = yield* reviewDiscard(field);
        yield* database.drizzle.update(environmentDeployment).set({ status: "failed" }).where(eq(environmentDeployment.id, active.id));
        const moved = yield* Effect.flip(discardEnvironmentChanges(actor, beforeFailure));
        assert.strictEqual(moved._tag, "Conflict");
        assert.strictEqual((yield* loadEnvironmentDocument(scope.environmentId)).intent.services[0]?.config.replicas, 7);
        yield* discardEnvironmentChanges(actor, yield* reviewDiscard(field));
        const afterFailure = yield* loadEnvironmentDocument(scope.environmentId);
        const afterFailureSaved = yield* loadLatestEnvironmentSavedState(scope.environmentId);
        assert.strictEqual(afterFailure.intent.services[0]?.config.replicas, 1);
        assert.strictEqual(afterFailureSaved?.intent.services[0]?.config.replicas, 1);
        assert.strictEqual(afterFailure.intent.services[0]?.config.name, "Later edit");
        const discardWrites = yield* database.drizzle.execute<{ txid: string }>(sql`
          select xmin::text as txid from environment where id = ${scope.environmentId}
          union all select xmin::text as txid from environment_saved_state_snapshot where id = ${afterFailureSaved?.id}`, "objects");
        assert.strictEqual(discardWrites.length, 2);
        assert.strictEqual(new Set(discardWrites.map(row => row.txid)).size, 1);

        yield* discardEnvironmentChanges(actor, yield* reviewDiscard({ kind: "node", nodeType: "service", nodeId: serviceId }));
        assert.deepStrictEqual((yield* loadCurrentEnvironmentState(scope.environmentId)).intent, canonicalizeEnvironmentIntent(baseline));
        yield* deleteVolumeResource(actor, { ...scope, revision: yield* revision(), resourceId: volume.data.resource.id });
        yield* discardEnvironmentChanges(actor, yield* reviewDiscard({ kind: "all" }));
        assert.deepStrictEqual((yield* loadCurrentEnvironmentState(scope.environmentId)).intent, canonicalizeEnvironmentIntent(baseline));
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
