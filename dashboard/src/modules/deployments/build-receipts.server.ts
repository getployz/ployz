import "@tanstack/react-start/server-only";
import type { BuildReceipts, MachineId } from "@ployz/sdk";
import { and, desc, eq, inArray, isNotNull, isNull, ne } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { DeploymentExecutionError } from "./execution-error";
import type { DeploymentContext } from "./runtime-repository.contract";
import { environmentDeployment, environmentDeploymentSecret } from "./tables";

const buildReceiptsSchema = Schema.Record(Schema.String, Schema.Struct({
  fingerprint: Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/)),
  machine_id: Schema.declare<MachineId>((value): value is MachineId =>
    Schema.is(rustMachineIdSchema)(value)),
  image: Schema.Struct({
    reference: Schema.String.check(Schema.isPattern(/^sha256:[0-9a-f]{64}$/)),
    tags: Schema.mutable(Schema.Array(Schema.String)),
    platforms: Schema.mutable(Schema.Array(Schema.String)),
    location: Schema.String,
  }),
}));

/** The latest complete preparation carries both reused and newly built images. */
export const loadBuildReceipts = Effect.fn("Deployments.loadBuildReceipts")(function* (context: DeploymentContext) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const [previous] = yield* drizzle.select({ receipts: environmentDeploymentSecret.encryptedBuildReceipts })
    .from(environmentDeploymentSecret)
    .innerJoin(environmentDeployment, eq(environmentDeployment.id, environmentDeploymentSecret.environmentDeploymentId))
    .where(and(
      eq(environmentDeployment.organizationId, context.organization.id),
      eq(environmentDeployment.environmentId, context.environment.id),
      ne(environmentDeployment.id, context.deployment.id),
      isNotNull(environmentDeploymentSecret.encryptedBuildReceipts),
    )).orderBy(desc(environmentDeployment.createdAt), desc(environmentDeployment.id)).limit(1);
  const encrypted = previous?.receipts;
  if (!encrypted) return {};
  return yield* Effect.try({
    try: () => Schema.decodeUnknownSync(buildReceiptsSchema)(JSON.parse(encryption.decrypt(encrypted)), { onExcessProperty: "error" }),
    catch: () => new DeploymentExecutionError({ failureCode: "build_receipts_invalid", message: "Completed build evidence could not be read." }),
  });
});

/** Build evidence is private: fingerprints include effective secret build variables. */
export const persistBuildReceipts = Effect.fn("Deployments.persistBuildReceipts")(function* (
  context: DeploymentContext, receipts: BuildReceipts,
) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const decoded = yield* Schema.decodeUnknownEffect(buildReceiptsSchema)(receipts, { onExcessProperty: "error" }).pipe(
    Effect.mapError(() => new DeploymentExecutionError({ failureCode: "build_receipts_invalid", message: "Completed build evidence is invalid." })),
  );
  const owned = drizzle.select({ id: environmentDeployment.id }).from(environmentDeployment).where(and(
    eq(environmentDeployment.id, context.deployment.id),
    eq(environmentDeployment.environmentId, context.environment.id),
    eq(environmentDeployment.organizationId, context.organization.id),
    eq(environmentDeployment.status, "deploying"),
    context.deployment.inngestRunId ? eq(environmentDeployment.inngestRunId, context.deployment.inngestRunId) : isNull(environmentDeployment.inngestRunId),
  ));
  const rows = yield* drizzle.update(environmentDeploymentSecret)
    .set({ encryptedBuildReceipts: encryption.encrypt(JSON.stringify(decoded)) })
    .where(inArray(environmentDeploymentSecret.environmentDeploymentId, owned))
    .returning({ id: environmentDeploymentSecret.environmentDeploymentId });
  if (rows.length === 0) return yield* new DeploymentExecutionError({
    failureCode: "build_receipts_not_owned", message: "Deployment no longer owns completed build evidence.",
  });
});
