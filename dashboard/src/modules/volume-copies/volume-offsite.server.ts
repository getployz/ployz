import "@tanstack/react-start/server-only";
import type { EnvironmentRef, OffsiteBucket, OffsiteStatus, SnapshotRefusal, VolumeSwitchReply, VolumeSwitchRequest } from "@ployz/sdk";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { observePlacement } from "#/modules/volume-placement/volume-placement";
import { environmentVolumes } from "#/modules/volume-placement/volume-placement.deploy.server";
import { observeVolumeMembers, resolveVolume, snapshotRefusalOf } from "#/modules/volume-run/volume-run.server";
import { NotFound } from "#/server/public-error";
import {
  markUploaded, offsiteVolumeIds, projectBuckets, saveBucket, saveVolumeOffsite, sealedBucket, volumeOffsiteRow, wireBucket,
} from "./offsite-bucket.server";
import {
  type Backup, type BackupChoice, backupFromWire, dailyBackupId, goneBackups, type OffsiteBucketView, offsiteServed, uploadBackupId,
} from "./volume-offsite";

type Refused = { readonly ok: false; readonly refusal: StoreRefusal };

const refused = (code: StoreRefusal["code"], message: string): Refused => ({ ok: false, refusal: { code, message, details: null } });

const refusalMessage = (reason: SnapshotRefusal, volume: string, server: string) => ({
  mismatch: "The volume moved. Try again.",
  no_writer: `${volume} has no writer on ${server}`,
  busy: `${volume} is busy. Try again.`,
  not_found: "Backup not found",
})[reason];

type OffsiteRequest = Extract<VolumeSwitchRequest, { command: "set_volume_offsite" | "inspect_volume_offsite" | "check_offsite" }>;
type Answer<R extends OffsiteRequest> = { readonly ok: true; readonly reply: VolumeSwitchReply<R["command"]> } | { readonly ok: false; readonly message: string };

/** The Volume's writer, reached through a session that stays open while the caller's work runs. */
type OffsiteWriter = {
  readonly volumeName: string;
  readonly environmentId: string;
  readonly projectId: string;
  /** The writer's newest snapshot, the only source an upload can name; null until a mirror round has taken one. */
  readonly newest: string | null;
  /** The request carries the bucket's secret key, so neither it nor the SDK's failure for it is logged. */
  readonly ask: <R extends OffsiteRequest>(request: (name: string) => R) => Effect.Effect<Answer<R>>;
};

const projectIdOf = Effect.fn("VolumeOffsite.projectId")(function* (organizationId: string, environment: EnvironmentRef) {
  const { environment: summary } = yield* readStore(organizationId, { query: "volumes", environment });
  const { projects } = yield* readStore(organizationId, { query: "projects" });
  const project = projects.find((entry) => entry.name === summary.project);
  if (project === undefined) return yield* new NotFound({ message: `No project ${summary.project}.` });
  return project.id;
});

const onOffsiteWriter = <A, E, R>(
  organizationId: string,
  environment: EnvironmentRef,
  volumeId: string,
  use: (writer: OffsiteWriter) => Effect.Effect<A, E, R>,
) => Effect.gen(function* () {
  const volume = yield* resolveVolume(organizationId, environment, volumeId);
  const members = yield* observeVolumeMembers(organizationId, volume.dockerVolume);
  const primary = observePlacement(members).primary;
  if (primary === null) return refused("conflict", `${volume.volumeName} has no writer`);
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return refused("unavailable", `The cluster is ${session.status}.`);
  const frame = yield* session.connected.watchFirstFrame(5_000);
  const machines = frame.machines.map((entry) => ({ id: entry.machine.id, capabilities: entry.machine.runtime.capabilities ?? [] }));
  if (!offsiteServed(machines, primary.id)) return refused("conflict", `Offsite needs an upgrade on ${primary.name}`);
  const member = members.find((entry) => entry.answered && entry.machine.id === primary.id);
  const copy = member?.answered === true ? member.view.copy : null;
  return yield* use({
    volumeName: volume.volumeName,
    environmentId: volume.environmentId,
    projectId: yield* projectIdOf(organizationId, environment),
    newest: copy?.kind === "root" ? copy.newest?.guid ?? null : null,
    ask: (request) => session.connected.volumeSwitch(primary.id, request(volume.dockerVolume)).pipe(
      Effect.map((reply) => ({ ok: true as const, reply })),
      Effect.catch((error) => {
        const reason = snapshotRefusalOf(error);
        const message = reason === null ? sdkFailureMessage(error, [primary]) : refusalMessage(reason, volume.volumeName, primary.name);
        return Effect.succeed({ ok: false as const, message });
      }),
    ),
  });
}).pipe(Effect.scoped);

export type OffsiteCopy = {
  readonly bucket: OffsiteBucketView;
  readonly keep: number;
  readonly uploadedAt: Date | null;
  /** Complete backups, newest first; null when the writer could not list them. */
  readonly backups: readonly Backup[] | null;
};

export type OffsiteRead =
  | { readonly served: false }
  | {
    readonly served: true;
    readonly buckets: readonly OffsiteBucketView[];
    readonly copy: OffsiteCopy | null;
    /** The snapshot Upload now sends; null leaves Upload now off. */
    readonly source: string | null;
  };

/** The Volume's offsite copy and its project's buckets; never a secret key, only that one is set. */
export const readVolumeOffsite = Effect.fn("VolumeOffsite.read")(function* (organizationId: string, environment: EnvironmentRef, volumeId: string) {
  const result = yield* onOffsiteWriter(organizationId, environment, volumeId, (writer) => Effect.gen(function* () {
    const buckets = yield* projectBuckets(organizationId, writer.projectId);
    const row = yield* volumeOffsiteRow(organizationId, volumeId);
    if (row === null) return { served: true, buckets, copy: null, source: writer.newest } satisfies OffsiteRead;
    const bucket = yield* sealedBucket(organizationId, row.bucket.id);
    const listed = yield* writer.ask((name) => ({ command: "inspect_volume_offsite", payload: { name, bucket } }));
    return {
      served: true,
      buckets,
      copy: { bucket: row.bucket, keep: row.keep, uploadedAt: row.uploadedAt, backups: listed.ok ? listed.reply.backups.map(backupFromWire) : null },
      source: writer.newest,
    } satisfies OffsiteRead;
  }));
  return "refusal" in result ? { served: false } satisfies OffsiteRead : result;
});

export type UploadResult =
  | Refused
  | { readonly ok: false; readonly field: string }
  | { readonly ok: true; readonly from: string | null; readonly status: OffsiteStatus };

const upload = Effect.fn("VolumeOffsite.upload")(function* (
  organizationId: string, volumeId: string, writer: OffsiteWriter, bucket: OffsiteBucket, from: string,
  backup = { id: uploadBackupId(volumeId, from), note: "manual" },
) {
  const sent = yield* writer.ask((name) => ({
    command: "set_volume_offsite",
    payload: { name, bucket, backup: backup.id, want: { kind: "uploaded", from, note: backup.note } },
  }));
  if (!sent.ok) return refused("conflict", sent.message);
  if (sent.reply.status.kind === "done") yield* markUploaded(organizationId, volumeId, new Date());
  return { ok: true, from, status: sent.reply.status } satisfies UploadResult;
});

/**
 * Gives the Volume an offsite copy now and starts its first upload. A New bucket is checked from the writer before it
 * is stored, so a bucket the Machine can't write never lands in the project.
 */
export const addVolumeOffsite = Effect.fn("VolumeOffsite.add")(function* (
  organizationId: string, environment: EnvironmentRef, volumeId: string, choice: BackupChoice,
) {
  return yield* onOffsiteWriter(organizationId, environment, volumeId, (writer) => Effect.gen(function* () {
    let bucketId: string;
    if (choice.kind === "new") {
      const typed = wireBucket(choice);
      const checked = yield* writer.ask(() => ({ command: "check_offsite", payload: { bucket: typed } }));
      if (!checked.ok) return { ok: false, field: checked.message } satisfies UploadResult;
      bucketId = (yield* saveBucket(organizationId, writer.projectId, typed)).id;
    } else {
      const owned = (yield* projectBuckets(organizationId, writer.projectId)).some((bucket) => bucket.id === choice.id);
      if (!owned) return yield* new NotFound({ message: "No such offsite bucket." });
      bucketId = choice.id;
    }
    yield* saveVolumeOffsite(organizationId, writer.environmentId, volumeId, bucketId);
    if (writer.newest === null) return { ok: true, from: null, status: { kind: "done" } } satisfies UploadResult;
    return yield* upload(organizationId, volumeId, writer, yield* sealedBucket(organizationId, bucketId), writer.newest);
  }));
});

/** Upload now: `from` is the snapshot a running upload started from, so asking again names the same backup. */
export const uploadVolumeOffsite = Effect.fn("VolumeOffsite.uploadNow")(function* (
  organizationId: string, environment: EnvironmentRef, volumeId: string, from: string | null,
) {
  return yield* onOffsiteWriter(organizationId, environment, volumeId, (writer) => Effect.gen(function* () {
    const row = yield* volumeOffsiteRow(organizationId, volumeId);
    if (row === null) return refused("not_found", `${writer.volumeName} has no offsite copy`);
    const source = from ?? writer.newest;
    if (source === null) return refused("conflict", `${writer.volumeName} has no snapshot to upload yet`);
    return yield* upload(organizationId, volumeId, writer, yield* sealedBucket(organizationId, row.bucket.id), source);
  }));
});

export type OffsiteVolume = { readonly organizationId: string; readonly environment: EnvironmentRef; readonly volumeId: string };

export type DailyPass =
  | { readonly status: "skipped"; readonly message: string }
  | { readonly status: "running"; readonly from: string }
  | { readonly status: "settled"; readonly failed: string | null };

const skip = (message: string) =>
  Effect.logInfo("Offsite pass skipped.", { message }).pipe(Effect.as<DailyPass>({ status: "skipped", message }));

/**
 * The day's upload for one Volume. `from` is null on the first ask, which takes the writer's newest snapshot; a resend
 * names the snapshot the first ask took, so the backup id and its source stay one pair.
 */
export const dailyUpload = Effect.fn("VolumeOffsite.daily")(function* (volume: OffsiteVolume, tick: Date, from: string | null) {
  const result = yield* onOffsiteWriter(volume.organizationId, volume.environment, volume.volumeId, (writer) => Effect.gen(function* () {
    const row = yield* volumeOffsiteRow(volume.organizationId, volume.volumeId);
    if (row === null) return yield* skip(`${writer.volumeName} has no offsite copy`);
    const source = from ?? writer.newest;
    if (source === null) return yield* skip(`${writer.volumeName} has no snapshot to upload yet`);
    const bucket = yield* sealedBucket(volume.organizationId, row.bucket.id);
    const sent = yield* upload(volume.organizationId, volume.volumeId, writer, bucket, source, { id: dailyBackupId(volume.volumeId, tick), note: "daily" });
    if (!sent.ok) return yield* skip(sent.refusal.message);
    if (sent.status.kind === "running") return { status: "running", from: source } satisfies DailyPass;
    return { status: "settled", failed: sent.status.kind === "failed" ? sent.status.reason : null } satisfies DailyPass;
  }));
  return "refusal" in result ? yield* skip(result.refusal.message) : result;
});

/** Deletes the backups past the copy's `keep`, newest kept; a refusal stops the prune and logs it. */
export const pruneOffsite = Effect.fn("VolumeOffsite.prune")(function* (volume: OffsiteVolume) {
  const result = yield* onOffsiteWriter(volume.organizationId, volume.environment, volume.volumeId, (writer) => Effect.gen(function* () {
    const row = yield* volumeOffsiteRow(volume.organizationId, volume.volumeId);
    if (row === null) return [];
    const bucket = yield* sealedBucket(volume.organizationId, row.bucket.id);
    const listed = yield* writer.ask((name) => ({ command: "inspect_volume_offsite", payload: { name, bucket } }));
    if (!listed.ok) {
      yield* Effect.logInfo("Offsite prune skipped.", { message: listed.message });
      return [];
    }
    const deleted: string[] = [];
    for (const backup of goneBackups(listed.reply.backups.map(backupFromWire), row.keep)) {
      const gone = yield* writer.ask((name) => ({ command: "set_volume_offsite", payload: { name, bucket, backup, want: { kind: "gone" } } }));
      if (!gone.ok || gone.reply.status.kind === "failed") {
        yield* Effect.logInfo("Offsite prune stopped.", { backup, message: gone.ok ? gone.reply.status.kind : gone.message });
        break;
      }
      deleted.push(backup);
    }
    return deleted;
  }));
  if ("refusal" in result) {
    yield* Effect.logInfo("Offsite prune skipped.", { message: result.refusal.message });
    return [];
  }
  return result;
});

/** Every Volume with an offsite copy, where the Store still lists it; an organization whose Store can't be read waits. */
export const offsiteVolumes = Effect.fn("VolumeOffsite.volumes")(function* () {
  const rows = yield* offsiteVolumeIds();
  const organizations = new Set(rows.map((row) => row.organizationId));
  const found: OffsiteVolume[] = [];
  for (const organizationId of organizations) {
    const known = yield* environmentVolumes(organizationId, null).pipe(
      Effect.catch((error) => Effect.logWarning("Offsite schedule could not read the Store.", { organizationId, error }).pipe(Effect.as([]))),
    );
    for (const row of rows.filter((entry) => entry.organizationId === organizationId)) {
      const volume = known.find((entry) => entry.volumeId === row.volumeId);
      if (volume !== undefined) found.push({ organizationId, environment: volume.environment, volumeId: row.volumeId });
    }
  }
  return found;
});
