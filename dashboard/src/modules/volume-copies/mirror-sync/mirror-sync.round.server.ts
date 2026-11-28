import "@tanstack/react-start/server-only";
import type { CopyObservation } from "@ployz/sdk";
import { Effect, Option } from "effect";
import { owners as allOwners, ownerOf } from "#/modules/config-store/store-lookup.server";
import { type MirrorSyncAnswer, syncMirrors } from "#/modules/volume-copies/mirror-sync/mirror-sync.server";
import { clusterRead, type ConnectedRuntimeClient, OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { placementRefusalIn } from "#/modules/volume-placement/volume-placement.gates.server";
import { ownedVolumes } from "#/modules/volume-placement/volume-placement.deploy.server";

const NO_COPIES: CopyObservation["copies"] = [];

/** The copies a session's Servers hold. The session's own read, unless a test provides another. */
export type SessionCopies = (session: ConnectedRuntimeClient) => Effect.Effect<CopyObservation["copies"], unknown>;
const sessionCopies: SessionCopies = (session) => session.observeCopies().pipe(Effect.map((observed) => observed.copies));

/**
 * The copies the Cluster's Servers hold and why the organization can't stage a placement now, both read over one
 * session. A Cluster with no copies to sync is never asked for a frame. The connect, the copies, the frame and the
 * close share 5 seconds of Cluster time; copies that don't come within it refresh nothing, and a frame that doesn't
 * refuses as unanswered.
 */
const lookAtCluster = Effect.fn("MirrorSync.lookAtCluster")(function* (organizationId: string, observe: SessionCopies) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId).pipe(
    Effect.catch((error) => Effect.succeed({ status: "unreachable" as const, error })),
  );
  // An unpaired Cluster has no Servers to ask, so it holds no copies to sync.
  if (session.status === "no_connection") return { copies: NO_COPIES, refusal: null };
  if (session.status === "unreachable") {
    yield* Effect.logWarning("Mirror refresh could not observe the copies.", { organizationId, error: session.error });
    return { copies: NO_COPIES, refusal: null };
  }
  const copies = yield* observe(session.connected).pipe(
    clusterRead,
    Effect.catch((error) => Effect.logWarning("Mirror refresh could not observe the copies.", { organizationId, error }).pipe(Effect.as(NO_COPIES))),
  );
  if (copies.length === 0) return { copies, refusal: null };
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(
    clusterRead,
    Effect.catch(() => Effect.succeed(null)),
  );
  return { copies, refusal: placementRefusalIn(frame) };
}, Effect.scoped, withClusterBudget(5_000));

/** A Volume a sync has something to say about: one with a slot anywhere, or with two primaries to settle. */
const syncs = (held: CopyObservation["copies"]) =>
  held.some((copy) => copy.role === "slot") || held.filter((copy) => copy.role === "writer").length > 1;

/**
 * Refresh every mirror in the organization (or one Environment): one mirror sync for each Volume with a slot or with
 * two primaries. Every sync logs what it did; a Volume whose sync could not run logs why.
 */
export const refreshMirrors = Effect.fn("MirrorSync.refreshMirrors")(function* (
  organizationId: string, onlyEnvironmentId: string | null, observe: SessionCopies = sessionCopies,
) {
  // A Deploy whose Environment holds no Volume has no mirror to refresh, so it doesn't wait for the Cluster's copies.
  if (onlyEnvironmentId !== null) {
    const owned = yield* ownerOf(organizationId, onlyEnvironmentId);
    if (owned === null) return [];
    const volumes = yield* ownedVolumes(organizationId, owned).pipe(Effect.option);
    if (Option.isSome(volumes) && volumes.value.length === 0) return [];
  }
  const { copies, refusal } = yield* lookAtCluster(organizationId, observe);
  if (copies.length === 0) return [];
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
