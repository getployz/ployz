import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import type { EnvironmentRef, SnapshotRefusal, VolumeSnapshots, VolumeSwitchRequest, WantedSnapshot } from "@ployz/sdk";
import { and, eq, isNull, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { observePlacement } from "#/modules/volume-placement/volume-placement";
import { observeVolumeMembers, resolveVolume, snapshotRefusalOf } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";
import { type SnapshotEdit, snapshotsServed } from "./volume-snapshots";

/** Why the writer did not answer: its own refusal, a writer without both snapshot capabilities, or anything else. */
export type SnapshotsFailure = SnapshotRefusal | "unsupported" | null;

export type SnapshotsResult =
  | { readonly ok: true; readonly snapshots: VolumeSnapshots }
  | { readonly ok: false; readonly refusal: StoreRefusal; readonly reason: SnapshotsFailure };

const refused = (reason: SnapshotsFailure, code: StoreRefusal["code"], message: string): SnapshotsResult =>
  ({ ok: false, reason, refusal: { code, message, details: null } });

const refusalMessage = (reason: SnapshotRefusal, volume: string, server: string) => ({
  mismatch: "The volume moved. Try again.",
  no_writer: `${volume} has no writer on ${server}`,
  busy: `${volume} is busy. Try again.`,
  not_found: "Snapshot not found",
})[reason];

const wanted = (edit: SnapshotEdit): WantedSnapshot => {
  switch (edit.kind) {
    case "take":
      return { kind: "taken", id: randomUUID(), label: { kind: "manual", note: "" }, kept: false };
    case "keep":
      return { kind: "kept", id: edit.id, kept: edit.kept };
    case "delete":
      return { kind: "deleted", id: edit.id };
  }
};

/** Sends one snapshot verb to the Volume's writer, the one Server whose snapshots are the Volume's. */
const onWriter = Effect.fn("VolumeSnapshots.onWriter")(function* (
  organizationId: string,
  environment: EnvironmentRef,
  volumeId: string,
  request: (name: string) => Extract<VolumeSwitchRequest, { command: "list_volume_snapshots" | "set_volume_snapshot" }>,
) {
  const volume = yield* resolveVolume(organizationId, environment, volumeId);
  const members = yield* observeVolumeMembers(organizationId, volume.dockerVolume);
  const writer = observePlacement(members).primary;
  if (writer === null) return refused("no_writer", "conflict", `${volume.volumeName} has no writer`);
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return refused(null, "unavailable", `The cluster is ${session.status}.`);
  const frame = yield* session.connected.watchFirstFrame(5_000);
  const machines = frame.machines.map((entry) => ({ id: entry.machine.id, capabilities: entry.machine.runtime.capabilities ?? [] }));
  if (!snapshotsServed(machines, writer.id)) return refused("unsupported", "conflict", `Snapshots need an upgrade on ${writer.name}`);
  return yield* session.connected.volumeSwitch(writer.id, request(volume.dockerVolume)).pipe(
    Effect.map((snapshots): SnapshotsResult => ({ ok: true, snapshots })),
    Effect.catch((error) => {
      const reason = snapshotRefusalOf(error);
      const message = reason === null ? sdkFailureMessage(error, [writer]) : refusalMessage(reason, volume.volumeName, writer.name);
      return Effect.succeed(refused(reason, reason === "not_found" ? "not_found" : "conflict", message));
    }),
  );
}, Effect.scoped);

export const listVolumeSnapshots = (organizationId: string, environment: EnvironmentRef, volumeId: string) =>
  onWriter(organizationId, environment, volumeId, (name) => ({ command: "list_volume_snapshots", payload: { name } }));

/** One Volume's snapshots on its writer: list them, or change one at the epoch they were listed at. */
export type SnapshotWriter<E, R> = {
  readonly list: Effect.Effect<SnapshotsResult, E, R>;
  readonly set: (expect: number, want: WantedSnapshot) => Effect.Effect<SnapshotsResult, E, R>;
};

export const snapshotWriter = (organizationId: string, environment: EnvironmentRef, volumeId: string) => ({
  list: listVolumeSnapshots(organizationId, environment, volumeId),
  set: Effect.fn("VolumeSnapshots.set")(function* (expect: number, want: WantedSnapshot) {
    const result = yield* onWriter(organizationId, environment, volumeId, (name) => ({ command: "set_volume_snapshot", payload: { name, expect, want } }));
    if (result.ok && want.kind === "deleted") yield* dropStagedRestore(organizationId, volumeId, want.id);
    return result;
  }),
});

/** Takes, keeps or deletes a snapshot now; `expect` is the writer epoch the snapshots were listed at. */
export const setVolumeSnapshot = (organizationId: string, environment: EnvironmentRef, volumeId: string, expect: number, edit: SnapshotEdit) =>
  snapshotWriter(organizationId, environment, volumeId).set(expect, wanted(edit));

/** A staged restore from a deleted snapshot has nothing left to put back, so the next Deploy must not try. */
export const dropStagedRestore = Effect.fn("VolumeSnapshots.dropStagedRestore")(function* (organizationId: string, volumeId: string, snapshotId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement)
    .set({ staged: sql`${volumePlacement.staged} - 'restore'`, updatedAt: new Date() })
    .where(and(
      eq(volumePlacement.organizationId, organizationId),
      eq(volumePlacement.volumeId, volumeId),
      isNull(volumePlacement.deploymentId),
      sql`${volumePlacement.staged} -> 'restore' ->> 'from' = ${snapshotId}`,
    ));
});
