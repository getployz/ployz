import type { Change, ConfigCommand, DiffView, EnvironmentRef, VolumeKind } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { settingText } from "./store-services";

/** Mounts `volume` on `service` at `path`, or detaches it (`null`), which keeps the Volume and its data. */
export function mountChange(service: string, volume: string, path: string | null): Change {
  const at = `${service}.mounts.${volume}`;
  return path === null ? { op: "unset", path: at } : { op: "set", path: at, value: path };
}

/** Mounts of `volume` that the next Deploy removes, by Service and deployed path: detached, their data kept. */
export function detachedMounts(diff: DiffView, volume: string) {
  return diff.changes.flatMap((node) => node.type !== "service" ? [] : node.settings.flatMap((row) =>
    row.path === `${node.name}.mounts.${volume}` && row.after === null && row.before !== null
      ? [{ service: node.name, path: settingText(row.before) }]
      : []));
}

/** Why a mount path won't do, before the Store says so: it must be absolute. */
export function mountPathError(path: string) {
  return path.startsWith("/") ? null : "Use an absolute path, such as /data.";
}

/** The command that creates a Volume with the id the caller minted, mounted nowhere yet. */
export function createVolumeCommand(id: string, environment: EnvironmentRef, name: string, storage: VolumeKind): ConfigCommand & { command: "create_volume" } {
  // The Store checks the id is a UUID.
  return { command: "create_volume", id, environment, name, storage, mounts: [] };
}

const GB = 1_000_000_000;

/**
 * Invalid managed limits never become an implicit Docker opt-out. GB are decimal and read exactly, to the byte: no
 * floating point between what was typed and what is stored.
 */
export function volumeStorage(managed: boolean, sizeGB: string): VolumeKind | null {
  if (!managed) return { kind: "docker" };
  const typed = /^(\d+)(?:\.(\d{1,9}))?$/.exec(sizeGB.trim());
  if (!typed) return null;
  const maximumBytes = Number(typed[1]) * GB + Number((typed[2] ?? "").padEnd(9, "0"));
  return Number.isSafeInteger(maximumBytes) && maximumBytes >= 1_000_000 ? { kind: "provisioned", maximumBytes } : null;
}

/** A byte count in decimal GB, exactly: 1001000000 is "1.001". */
export function gigabytes(bytes: number) {
  const fraction = String(bytes % GB).padStart(9, "0").replace(/0+$/, "");
  return fraction ? `${Math.floor(bytes / GB)}.${fraction}` : String(Math.floor(bytes / GB));
}

export function volumeStorageText(storage: VolumeKind) {
  return storage.kind === "provisioned" ? `${gigabytes(storage.maximumBytes)} GB limit` : "Docker volume";
}

const decodeVolumeLoss = Schema.decodeUnknownOption(Schema.Struct({
  volumes: Schema.Array(Schema.Struct({
    name: Schema.String,
    deletes: Schema.Array(Schema.Struct({ machine_id: Schema.String })),
  })),
  accept: Schema.Array(Schema.String),
  version: Schema.String,
}));

/**
 * What a Deploy the Store refused with `confirmation_required` would delete: each Volume whose data the Servers hold,
 * with those Servers, and what to send back to accept exactly that. Null for any other refusal.
 */
export function volumeLoss(refusal: { code: string; details: unknown }) {
  if (refusal.code !== "confirmation_required") return null;
  return Option.getOrNull(decodeVolumeLoss(refusal.details));
}

export type VolumeLoss = NonNullable<ReturnType<typeof volumeLoss>>;
