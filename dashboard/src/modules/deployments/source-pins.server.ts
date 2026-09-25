import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { environmentDeployment } from "./tables";
import { environmentNodeConfigSnapshot } from "#/modules/runtime/tables";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "./runtime-contract";
import { deploymentSourcePinsSchema, validateDeploymentSourcePins } from "./source-pins";

/** Resolve outside this transaction, then persist before source download. Existing pins never rotate. */
export const persistDeploymentSourcePin = Effect.fn("Deployments.persistSourcePin")(
  function* (input: {
    organizationId: string;
    environmentDeploymentId: string;
    inngestRunId: string;
    serviceId: string;
    commitSha: string;
  }) {
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [attempt] = yield* drizzle.select().from(environmentDeployment).where(and(
        eq(environmentDeployment.id, input.environmentDeploymentId),
        eq(environmentDeployment.organizationId, input.organizationId),
        eq(environmentDeployment.inngestRunId, input.inngestRunId),
        // Image Builds run before deploying, so any active status the run owns may pin.
        inArray(environmentDeployment.status, [...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES]),
        isNull(environmentDeployment.cancellationRequestedAt),
      )).for("update");
      if (!attempt) return yield* new Conflict({ message: "Deployment no longer owns source acquisition." });
      const snapshots = yield* drizzle.select().from(environmentNodeConfigSnapshot)
        .where(eq(environmentNodeConfigSnapshot.environmentDeploymentId, attempt.id));
      const decoded = yield* Schema.decodeUnknownEffect(deploymentSourcePinsSchema)(
        { [input.serviceId]: { commitSha: input.commitSha } }, { onExcessProperty: "error" },
      ).pipe(Effect.mapError(() => new Conflict({ message: "Deployment source pins are invalid." })));
      const pins = yield* validateDeploymentSourcePins(decoded, snapshots);
      const previous = attempt.sourcePins[input.serviceId];
      if (previous && previous.commitSha !== input.commitSha) {
        return yield* new Conflict({ message: "A captured deployment commit cannot be replaced." });
      }
      const sourcePins = { ...attempt.sourcePins, ...pins };
      yield* drizzle.update(environmentDeployment).set({ sourcePins, updatedAt: new Date() })
        .where(eq(environmentDeployment.id, attempt.id));
      return sourcePins;
    }));
  },
);
