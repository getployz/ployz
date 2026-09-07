import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNotNull } from "drizzle-orm";
import { Effect } from "effect";
import {
  environmentResource as schemaEnvironmentResource,
} from "#/modules/environment-design/tables";
import { Database } from "#/server/database.server";
import { Conflict, Validation } from "#/server/public-error";
import { getVolumePhysicalName } from "#/modules/environment-design/volume-config";
import {
  destructiveVolumeActions,
} from "#/modules/operations/destructive-volume-preparation.server";
import type { DestructiveVolumeReview } from "#/modules/deployments/deployment-contract";
import type { Actor } from "#/modules/identity/actor";

export const gatherExactTombstonedVolumeReviews = Effect.fn(
  "Deployments.gatherExactTombstonedVolumeReviews",
)(function* (input: {
  readonly actor: Actor;
  readonly organizationSlug: string;
  readonly environmentId: string;
  readonly resourceIds: readonly string[];
}) {
  const { drizzle: database } = yield* Database;
  const resourceIds = [...new Set(input.resourceIds)];
  if (resourceIds.length === 0) {
    return [] satisfies DestructiveVolumeReview[];
  }
  const tombstones = yield* database
        .select({ resourceId: schemaEnvironmentResource.id })
        .from(schemaEnvironmentResource)
        .where(
          and(
            eq(schemaEnvironmentResource.environmentId, input.environmentId),
            eq(schemaEnvironmentResource.implementationType, "volume"),
            isNotNull(schemaEnvironmentResource.deletedAt),
            inArray(schemaEnvironmentResource.id, resourceIds),
          ),
        );
  if (tombstones.length !== resourceIds.length) {
    return yield* Effect.fail(
      new Conflict({
        message:
          "The exact tombstoned volume set changed before destructive retry.",
      }),
    );
  }
  return yield* gatherTombstonedVolumeReviews(input, tombstones);
});

function gatherTombstonedVolumeReviews(
  input: {
    actor: Actor;
    organizationSlug: string;
    environmentId: string;
  },
  tombstoneRows: readonly { resourceId: string }[],
) {
  return Effect.gen(function* () {
    const tombstones = tombstoneRows.map(({ resourceId }) => ({
      resourceId,
      physicalName: getVolumePhysicalName(resourceId),
    }));
    if (tombstones.length === 0) return [] satisfies DestructiveVolumeReview[];

    const gathered = yield* destructiveVolumeActions.prepareNamespace(input);

    const evidenceByPhysicalName = new Map(
      gathered.evidence.volumes.map((volume) => [
        volume.evidence.volumeName,
        volume,
      ]),
    );
    const reviewedAt = new Date().toISOString();
    return yield* Effect.forEach(tombstones, (tombstone) => {
      const volume = evidenceByPhysicalName.get(tombstone.physicalName);
      if (!volume) {
        return Effect.fail(
          new Validation({
            message: `Runtime did not report deployed volume ${tombstone.physicalName}.`,
          }),
        );
      }
      return Effect.succeed({
        target: {
          version: 1 as const,
          resourceId: tombstone.resourceId,
          namespaceId: gathered.namespaceId,
          volumeName: tombstone.physicalName,
          machineId: volume.evidence.machineId,
        },
        evidence: {
          version: 1 as const,
          fingerprint: volume.fingerprint,
          reviewedAt,
          evidence: volume.evidence,
        },
      } satisfies DestructiveVolumeReview);
    });
  });
}
