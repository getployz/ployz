import "@tanstack/react-start/server-only";
import type { CopyObservation } from "@ployz/sdk";
import { Effect, Option } from "effect";
import { owners as allOwners, ownerOf } from "#/modules/config-store/store-lookup.server";
import { type MirrorSyncAnswer, syncMirrors } from "#/modules/volume-copies/mirror-sync/mirror-sync.server";
import { placementRefusal } from "#/modules/volume-placement/volume-placement.gates.server";
import { ownedVolumes } from "#/modules/volume-placement/volume-placement.deploy.server";
import { type CopyObserver, observeOrganizationCopies } from "#/modules/volume-run/volume-run.orphans.server";

const NO_COPIES: CopyObservation["copies"] = [];

/** A Volume a sync has something to say about: one with a slot anywhere, or with two primaries to settle. */
const syncs = (held: CopyObservation["copies"]) =>
  held.some((copy) => copy.role === "slot") || held.filter((copy) => copy.role === "writer").length > 1;

/**
 * Refresh every mirror in the organization (or one Environment): one mirror sync for each Volume with a slot or with
 * two primaries. Every sync logs what it did; a Volume whose sync could not run logs why.
 */
export const refreshMirrors = Effect.fn("MirrorSync.refreshMirrors")(function* (
  organizationId: string, onlyEnvironmentId: string | null, observe: CopyObserver = observeOrganizationCopies,
) {
  // A Deploy whose Environment holds no Volume has no mirror to refresh, so it doesn't wait for the Cluster's copies.
  if (onlyEnvironmentId !== null) {
    const owned = yield* ownerOf(organizationId, onlyEnvironmentId);
    if (owned === null) return [];
    const volumes = yield* ownedVolumes(organizationId, owned).pipe(Effect.option);
    if (Option.isSome(volumes) && volumes.value.length === 0) return [];
  }
  const copies = yield* observe(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Mirror refresh could not observe the copies.", { organizationId, error }).pipe(Effect.as(NO_COPIES))),
  );
  if (copies.length === 0) return [];
  const refusal = yield* placementRefusal(organizationId);
  if (refusal !== null) {
    yield* Effect.logInfo("Mirror refresh skipped.", { organizationId, reason: refusal.message });
    return [];
  }
  const owners = onlyEnvironmentId === null ? yield* allOwners(organizationId) : [yield* ownerOf(organizationId, onlyEnvironmentId)];
  const synced: MirrorSyncAnswer[] = [];
  for (const owned of owners) {
    if (owned === null) continue;
    // In the organization-wide pass, an Environment the Store can't read keeps its mirrors as they are; it stops no
    // other Environment's refresh. A Deploy's own Environment still fails its Deploy.
    const volumes = yield* ownedVolumes(organizationId, owned).pipe(
      Effect.catchTag("StoreRefused", (refused) => onlyEnvironmentId !== null ? Effect.fail(refused) : Effect.logWarning(
        "Mirror refresh skipped an Environment the Store can't read.",
        { organizationId, project: owned.project, environment: owned.environment, reason: refused.message },
      ).pipe(Effect.as([]))),
    );
    const answers = yield* Effect.forEach(volumes.filter((volume) => syncs(copies.filter((copy) => copy.name === volume.dockerVolume))), (volume) =>
      syncMirrors(organizationId, volume).pipe(
        Effect.catch((error) => Effect.logWarning(`could not sync ${volume.name}`, { organizationId, volumeId: volume.volumeId, error }).pipe(Effect.as(null))),
      ), { concurrency: "unbounded" });
    synced.push(...answers.filter((answer) => answer !== null));
  }
  return synced;
});
