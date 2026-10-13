import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import type { EnvironmentRef, SnapshotRefusal, VolumeSnapshots, VolumeSwitchRequest, WantedSnapshot } from "@ployz/sdk";
import { and, eq, isNull, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { observePlacement } from "#/modules/volume-placement/volume-placement";
import { observeVolumeMembers, resolveVolume } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";
import type { SnapshotEdit } from "./volume-snapshots";

export type SnapshotsResult = { readonly ok: true; readonly snapshots: VolumeSnapshots } | { readonly ok: false; readonly refusal: StoreRefusal };

const decodeRefusal = Schema.decodeUnknownOption(Schema.Struct({
  details: Schema.Struct({ reason: Schema.Literals(["mismatch", "no_writer", "busy", "not_found"]) }),
}));

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
  if (writer === null) {
    return { ok: false, refusal: { code: "conflict", message: `${volume.volumeName} has no writer`, details: null } } satisfies SnapshotsResult;
  }
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") {
    return { ok: false, refusal: { code: "unavailable", message: `The cluster is ${session.status}.`, details: null } } satisfies SnapshotsResult;
  }
  return yield* session.connected.volumeSwitch(writer.id, request(volume.dockerVolume)).pipe(
    Effect.map((snapshots): SnapshotsResult => ({ ok: true, snapshots })),
    Effect.catch((error) => {
      const refused = decodeRefusal("cause" in error ? error.cause : undefined);
      const message = Option.isSome(refused)
        ? refusalMessage(refused.value.details.reason, volume.volumeName, writer.name)
        : sdkFailureMessage(error, [writer]);
      const code = Option.isSome(refused) && refused.value.details.reason === "not_found" ? "not_found" : "conflict";
      return Effect.succeed<SnapshotsResult>({ ok: false, refusal: { code, message, details: null } });
    }),
  );
}, Effect.scoped);

export const listVolumeSnapshots = (organizationId: string, environment: EnvironmentRef, volumeId: string) =>
  onWriter(organizationId, environment, volumeId, (name) => ({ command: "list_volume_snapshots", payload: { name } }));

/** Takes, keeps or deletes a snapshot now; `expect` is the writer epoch the snapshots were listed at. */
export const setVolumeSnapshot = Effect.fn("VolumeSnapshots.set")(function* (
  organizationId: string,
  environment: EnvironmentRef,
  volumeId: string,
  expect: number,
  edit: SnapshotEdit,
) {
  const result = yield* onWriter(organizationId, environment, volumeId, (name) => ({
    command: "set_volume_snapshot",
    payload: { name, expect, want: wanted(edit) },
  }));
  if (result.ok && edit.kind === "delete") yield* dropStagedRestore(organizationId, volumeId, edit.id);
  return result;
});

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
