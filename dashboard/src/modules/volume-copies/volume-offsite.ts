import type { OffsiteBackup } from "@ployz/sdk";
import { requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import { digest } from "./snapshot-schedule";

/** A project's bucket as the browser sees it: the secret key never leaves the server, only that it is set. */
export type OffsiteBucketView = {
  readonly id: string;
  readonly endpoint: string;
  readonly region: string;
  readonly bucket: string;
  readonly prefix: string;
  readonly accessKey: string;
  readonly secretSet: true;
};

/** A bucket typed into Add copy, before Cloud checks and stores it. */
export type NewBucket = {
  readonly endpoint: string;
  readonly bucket: string;
  readonly accessKey: string;
  readonly secretKey: string;
};

export type BackupChoice = { readonly kind: "existing"; readonly id: string } | ({ readonly kind: "new" } & NewBucket);

export type Backup = {
  readonly id: string;
  readonly at: Date;
  readonly bytes: number;
  readonly note: string | null;
};

export const OFFSITE_KEEP = 7;
export const OFFSITE_PREFIX = "ployz";

export const backupFromWire = (wire: OffsiteBackup): Backup => ({
  id: wire.id,
  at: new Date(wire.taken_unix_seconds * 1000),
  bytes: wire.bytes,
  note: wire.note === "" ? null : wire.note,
});

/** Offsite stays hidden unless the Volume's writer lists all three offsite capabilities. */
export function offsiteServed(machines: readonly { readonly id: string; readonly capabilities: readonly string[] }[], writer: string | null): boolean {
  const machine = machines.find((entry) => entry.id === writer);
  return machine !== undefined
    && serverSupports(machine, requirements.offsiteSet) === true
    && serverSupports(machine, requirements.offsiteInspect) === true
    && serverSupports(machine, requirements.offsiteCheck) === true;
}

/** One backup per source snapshot, so a resent upload names the backup it already started. */
export const uploadBackupId = (volumeId: string, guid: string) => `up-${digest(`${volumeId}/${guid}`)}`;

/** The day's backup, minted from the Volume and the UTC midnight tick like a daily snapshot, but its own namespace. */
export const dailyBackupId = (volumeId: string, tick: Date) => `off-${Math.floor(tick.getTime() / 1000)}-${digest(volumeId)}`;

/** The backups beyond the newest `keep`; the newest one always stays. */
export const goneBackups = (backups: readonly Pick<Backup, "id" | "at">[], keep: number): readonly string[] =>
  [...backups].sort((a, b) => b.at.getTime() - a.at.getTime()).slice(Math.max(keep, 1)).map((backup) => backup.id);

/** The wire wants a URL; people paste a host. */
export const endpointUrl = (endpoint: string) => {
  const trimmed = endpoint.trim().replace(/\/+$/, "");
  return /^https?:\/\//.test(trimmed) ? trimmed : `https://${trimmed}`;
};

/** AWS names the region in its host; other S3 stores accept any region, and us-east-1 is what they default to. */
export const regionOf = (endpoint: string) => /(?:^|[/.])s3[.-]([a-z0-9-]+)\.amazonaws\.com/.exec(endpoint)?.[1] ?? "us-east-1";

export const scheduleWords = (keep: number) => `Daily · ${keep} kept`;
