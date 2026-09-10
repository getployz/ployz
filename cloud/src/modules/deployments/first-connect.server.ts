import "@tanstack/react-start/server-only";

import { isDeepStrictEqual } from "node:util";
import { and, asc, eq, isNull, sql } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { environment as schemaEnvironment } from "#/modules/project/tables";
import { organizationPairing as schemaOrganizationPairing } from "#/modules/runtime/tables";
import type { EncryptedSecretValue } from "#/db/tables";
import {
  loadLatestEnvironmentSavedState,
} from "#/modules/environment-design/saved-state-repository.server";
import { admitEnvironmentDeployment } from "./admission.server";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";

type FirstConnectInput = {
  readonly organizationId: string;
  readonly machineId: string;
  readonly encryptedPairingSecret: EncryptedSecretValue;
};

function replayableFirstConnectDeployments(
  input: Pick<FirstConnectInput, "organizationId" | "machineId">,
) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle
      .select({
        environmentDeploymentId: schemaEnvironmentDeployment.id,
        environmentId: schemaEnvironmentDeployment.environmentId,
      })
      .from(schemaEnvironmentDeployment)
      .where(
        and(
          eq(
            schemaEnvironmentDeployment.organizationId,
            input.organizationId,
          ),
          eq(schemaEnvironmentDeployment.status, "queued"),
          isNull(schemaEnvironmentDeployment.inngestRunId),
          sql`${schemaEnvironmentDeployment.triggerOrigin}->>'origin' = 'first_connect'`,
          sql`${schemaEnvironmentDeployment.triggerOrigin}->>'machineId' = ${input.machineId}`,
        ),
      )
      .orderBy(asc(schemaEnvironmentDeployment.createdAt));
  });
}

/**
 * Atomically commits the founder identity and evaluates the one-shot
 * first-connect deployment policy. A replay returns only committed queued rows
 * still lacking an Inngest run owner, so the caller can safely dispatch again.
 */
export const commitFirstConnectAdmission = Effect.fn(
  "Deployments.commitFirstConnectAdmission",
)(function* (input: FirstConnectInput) {
  const { drizzle } = yield* Database;
  const pairingRows = yield* drizzle
    .select({
      encryptedPairingSecret:
        schemaOrganizationPairing.encryptedPairingSecret,
      founderMachineId: schemaOrganizationPairing.founderMachineId,
      removalStartedAt: schemaOrganizationPairing.removalStartedAt,
      firstConnectDeploymentEvaluatedAt:
        schemaOrganizationPairing.firstConnectDeploymentEvaluatedAt,
    })
    .from(schemaOrganizationPairing)
    .where(
      eq(
        schemaOrganizationPairing.organizationId,
        input.organizationId,
      ),
    )
    .for("update")
    .limit(1);
  const pairing = pairingRows[0];
  if (
    pairing === undefined ||
    pairing.removalStartedAt !== null ||
    !isDeepStrictEqual(
      pairing.encryptedPairingSecret,
      input.encryptedPairingSecret,
    )
  ) {
    return yield* new Conflict({
      message: "The founding attempt is no longer current.",
    });
  }
  if (
    pairing.founderMachineId !== null &&
    pairing.founderMachineId !== input.machineId
  ) {
    return yield* new Conflict({
      message: "The Organization is already ready on another Machine.",
    });
  }
  if (pairing.firstConnectDeploymentEvaluatedAt !== null) {
    return yield* replayableFirstConnectDeployments(input);
  }

  if (pairing.founderMachineId === null) {
    yield* drizzle
      .update(schemaOrganizationPairing)
      .set({ founderMachineId: input.machineId, updatedAt: new Date() })
      .where(
        and(
          eq(
            schemaOrganizationPairing.organizationId,
            input.organizationId,
          ),
          isNull(schemaOrganizationPairing.founderMachineId),
        ),
      );
  }

  const environments = yield* drizzle
    .select({ id: schemaEnvironment.id })
    .from(schemaEnvironment)
    .where(eq(schemaEnvironment.organizationId, input.organizationId))
    .orderBy(asc(schemaEnvironment.id));
  const admitted = yield* Effect.forEach(environments, (environment) =>
    Effect.gen(function* () {
      const saved = yield* loadLatestEnvironmentSavedState(environment.id);
      if (saved === null) return null;
      const deployment = yield* admitEnvironmentDeployment({
        environmentId: environment.id,
        savedStateSnapshotId: saved.id,
        triggerOrigin: { origin: "first_connect", machineId: input.machineId },
        message: null,
      });
      return {
        environmentDeploymentId: deployment.id,
        environmentId: environment.id,
      };
    }),
  );

  yield* drizzle
    .update(schemaOrganizationPairing)
    .set({ firstConnectDeploymentEvaluatedAt: new Date(), updatedAt: new Date() })
    .where(
      eq(
        schemaOrganizationPairing.organizationId,
        input.organizationId,
      ),
    );
  return admitted.filter((row) => row !== null);
});
