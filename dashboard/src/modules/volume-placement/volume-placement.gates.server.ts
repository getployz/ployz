import "@tanstack/react-start/server-only";
import type { EnvironmentRef } from "@ployz/sdk";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { type StoreRefusal, serversUnanswered } from "#/lib/refusal";
import { placementBlockers, placementUpgradeMessage } from "#/modules/runtime/daemon-release";
import { dockerVolumeName } from "#/modules/runtime/naming";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import type { Placement } from "#/modules/volume-placement/volume-placement";
import { originRefusal } from "#/modules/volume-run/volume-run.rows.server";

/** Staging refuses while a Server is silent or its report hasn't arrived, the frame gate's way and with its code. */
export const UNANSWERED = serversUnanswered("unanswered");

/** The names of the Servers with storage that lack the volume verbs. Fails unless every Server answers, since a silent one may run anything. */
export const readPlacementBlockers = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.flatMap((frame) => frame === null || frame.incomplete_ids.machines.length > 0 ? Effect.fail("not every Server answered") : Effect.succeed(placementBlockers(frame.machines.map((entry) => ({
    name: entry.machine.name, storage: entry.storage?.state ?? null, capabilities: entry.machine.runtime.capabilities ?? [],
  }))))),
);

/** Why the organization can't stage a placement or restore yet, or null when it can. */
export const placementRefusal = Effect.fn("VolumePlacement.refusal")(function* (organizationId: string) {
  const blockers = yield* readPlacementBlockers(organizationId).pipe(Effect.catch(() => Effect.succeed(null)));
  if (blockers === null) return UNANSWERED;
  if (blockers.length === 0) return null;
  const refusal: StoreRefusal = { code: "upgrade_required", message: placementUpgradeMessage(blockers), details: { servers: [...blockers] } };
  return refusal;
});

/** `write` runs only once the organization can stage; otherwise the refusal answers in its place. */
export const unlessPlacementRefused = <A, E, R>(organizationId: string, write: Effect.Effect<A, E, R>) =>
  Effect.gen(function* () {
    const refusal = yield* placementRefusal(organizationId);
    if (refusal !== null) return { ok: false, refusal } as const;
    return yield* write;
  });

/**
 * `write` runs only once every Server in `placement`, and every one that holds a copy now, can hold Volume `volumeId`:
 * any Server for a Legacy Volume, and only one that checks where a copy's data began for a born one.
 */
export const unlessOriginRefused = <A, E, R>(
  organizationId: string,
  { environment, volumeId, placement }: { readonly environment: EnvironmentRef; readonly volumeId: string; readonly placement: Placement },
  write: Effect.Effect<A, E, R>,
) => Effect.gen(function* () {
  const { environment: summary, volumes } = yield* readStore(organizationId, { query: "volumes", environment });
  const volume = volumes.find((entry) => entry.id === volumeId);
  if (volume?.born !== true) return yield* write;
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  const owned = namespaces.find((entry) => entry.project === summary.project && entry.environment === summary.name);
  const dockerVolume = owned === undefined ? null : dockerVolumeName(owned.namespace, volumeId);
  const refusal = yield* originRefusal(organizationId, { volumeName: volume.name, dockerVolume }, [placement.primary, ...placement.mirrors], UNANSWERED);
  if (refusal !== null) return { ok: false, refusal } as const;
  return yield* write;
});
