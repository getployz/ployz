import { loadEnvironmentSavedIntentById } from "#/modules/environment-design/saved-state-repository.server";
import "@tanstack/react-start/server-only";
import { and, asc, eq } from "drizzle-orm";
import { Effect, Result, Schema } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { service as schemaService } from "#/modules/environment-design/tables";
import { organization as schemaOrganization } from "#/modules/organization/tables";
import {
  environment as schemaEnvironment,
  project as schemaProject,
} from "#/modules/project/tables";
import {
  environmentNodeConfigSnapshot as schemaEnvironmentNodeConfigSnapshot,
} from "#/modules/runtime/tables";
import {
  getResolvedDeployEnvBySnapshotConfig,
} from "#/modules/deployments/deploy-environment.server";
import {
  loadEnvironmentSnapshotProjection,
} from "#/modules/deployments/environment-state.repository.server";
import { getResolvedHealthcheckPort } from "#/modules/deployments/runtime-contract";
import {
  decodeStrict,
  strictParseOptions,
} from "#/modules/environment-design/schema";
import { serviceDeploymentConfigSchema } from "#/modules/environment-design/services";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { type DeploymentContext } from "./runtime-repository.contract";

function redactedDeployEnv(config: DeploymentContext["snapshots"][number]["config"]) {
  return Object.fromEntries(
    Object.entries(config.env).map(([key, value]) => [
      key,
      value.kind === "secret" ? "[secret]" : value.value,
    ]),
  );
}

export const loadDeploymentContext = Effect.fn(
  "Deployments.loadDeploymentContext",
)((environmentDeploymentId: string) =>
  Effect.gen(function* () {
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    const [record] = yield* database.drizzle
      .select({
        deployment: schemaEnvironmentDeployment,
        environment: schemaEnvironment,
        project: schemaProject,
        organization: {
          id: schemaOrganization.id,
          slug: schemaOrganization.slug,
        },
      })
      .from(schemaEnvironmentDeployment)
      .innerJoin(
        schemaEnvironment,
        eq(schemaEnvironment.id, schemaEnvironmentDeployment.environmentId),
      )
      .innerJoin(schemaProject, eq(schemaProject.id, schemaEnvironment.projectId))
      .innerJoin(
        schemaOrganization,
        eq(schemaOrganization.id, schemaProject.organizationId),
      )
      .where(eq(schemaEnvironmentDeployment.id, environmentDeploymentId))
      .limit(1);

    if (!record) return null;
    const saved = yield* loadEnvironmentSavedIntentById({ environmentId: record.environment.id, savedStateSnapshotId: record.deployment.savedStateSnapshotId });
    if (!saved) throw new Error("Deployment authored intent is missing.");

    const [snapshotRows, volumeSnapshotRows, appliedProjection] =
      yield* Effect.all([
        database.drizzle
          .select({
            serviceId: schemaEnvironmentNodeConfigSnapshot.nodeId,
            config: schemaEnvironmentNodeConfigSnapshot.config,
          })
          .from(schemaEnvironmentNodeConfigSnapshot)
          .innerJoin(
            schemaService,
            eq(schemaService.id, schemaEnvironmentNodeConfigSnapshot.nodeId),
          )
          .where(
            and(
              eq(
                schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
                environmentDeploymentId,
              ),
              eq(schemaEnvironmentNodeConfigSnapshot.nodeType, "service"),
            ),
          )
          .orderBy(asc(schemaService.createdAt)),
        database.drizzle
          .select({
            volumeResourceId: schemaEnvironmentNodeConfigSnapshot.nodeId,
            config: schemaEnvironmentNodeConfigSnapshot.config,
          })
          .from(schemaEnvironmentNodeConfigSnapshot)
          .where(
            and(
              eq(
                schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
                environmentDeploymentId,
              ),
              eq(schemaEnvironmentNodeConfigSnapshot.nodeType, "volume"),
            ),
          ),
        loadEnvironmentSnapshotProjection({
          kind: "environment",
          environmentId: record.environment.id,
        }),
      ]);
    const snapshots = snapshotRows.map((snapshot) => ({
      serviceId: snapshot.serviceId,
      serviceSlug: saved.intent.services.find((node) => node.id === snapshot.serviceId)?.slug ?? snapshot.serviceId,
      config: decodeStrict(serviceDeploymentConfigSchema, snapshot.config),
    }));
    const resolvedEnvByServiceId = yield* getResolvedDeployEnvBySnapshotConfig(
      encryption,
      snapshots.map((snapshot) => ({
        serviceId: snapshot.serviceId,
        config: snapshot.config,
      })),
      record.deployment.variableProducers,
    );

    return {
      deployment: record.deployment,
      environment: record.environment,
      project: record.project,
      organization: record.organization,
      snapshots: snapshots.map((snapshot) => ({
        ...snapshot,
        resolvedEnv: redactedDeployEnv(snapshot.config),
        healthcheckPort: getResolvedHealthcheckPort(
          resolvedEnvByServiceId.get(snapshot.serviceId),
        ),
      })),
      appliedServiceIds:
        appliedProjection.explicitStates
          .find((state) => state.environmentId === record.environment.id)
          ?.applied.nodes.flatMap((node) => {
            if (node.nodeType !== "service") return [];
            const parsed = Schema.decodeUnknownResult(serviceDeploymentConfigSchema)(
              node.config,
              strictParseOptions,
            );
            return Result.isSuccess(parsed) ? [parsed.success.privateDns] : [];
          }) ?? [],
      volumes: volumeSnapshotRows.map((row) => ({
        volumeResourceId: row.volumeResourceId,
      })),
    } satisfies DeploymentContext;
  }));

export const loadResolvedDeployEnv = Effect.fn(
  "Deployments.loadResolvedDeployEnv",
)((context: DeploymentContext) =>
  Effect.gen(function* () {
    const encryption = yield* SecretEncryption;
    return yield* getResolvedDeployEnvBySnapshotConfig(
      encryption,
      context.snapshots.map((snapshot) => ({
        serviceId: snapshot.serviceId,
        config: snapshot.config,
      })),
      context.deployment.variableProducers ?? null,
    );
  }));
