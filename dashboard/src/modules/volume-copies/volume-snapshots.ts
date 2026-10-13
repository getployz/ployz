import type { UserSnapshot } from "@ployz/sdk";
import { requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import type { Snapshot, SnapshotKind } from "./volume-copies";

export const SNAPSHOT_WORDS = {
  manual: "Manual",
  hourly: "Hourly",
  daily: "Daily",
  before_deploy: "Before deploy",
  after_deploy: "After deploy",
  before_restore: "Before restore",
} as const satisfies Record<SnapshotKind, string>;

/** One snapshot verb that runs now on the writer, not on deploy. */
export type SnapshotEdit =
  | { readonly kind: "take" }
  | { readonly kind: "keep"; readonly id: string; readonly kept: boolean }
  | { readonly kind: "delete"; readonly id: string };

export function snapshotFromWire(wire: UserSnapshot): Snapshot {
  return {
    id: wire.id,
    at: new Date(wire.created_unix_seconds * 1000),
    kind: wire.label.kind,
    note: wire.label.note === "" ? null : wire.label.note,
    kept: wire.kept,
    bytes: wire.written_bytes,
    losesBytes: wire.loses_bytes,
  };
}

/** Snapshots stay hidden unless the Volume's writer lists both snapshot capabilities. */
export function snapshotsServed(machines: readonly { readonly id: string; readonly capabilities: readonly string[] }[], writer: string | null): boolean {
  const machine = machines.find((entry) => entry.id === writer);
  return machine !== undefined
    && serverSupports(machine, requirements.snapshots) === true
    && serverSupports(machine, requirements.snapshotList) === true;
}

export function snapshotClock(timeZone?: string) {
  const time = new Intl.DateTimeFormat("en-GB", { hourCycle: "h23", hour: "2-digit", minute: "2-digit", timeZone });
  const date = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", timeZone });
  const key = new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone });
  const dayBefore = (now: Date) => new Date(now.getTime() - 86_400_000);
  const day = (at: Date, now: Date) => {
    if (key.format(at) === key.format(now)) return "Today";
    if (key.format(at) === key.format(dayBefore(now))) return "Yesterday";
    return date.format(at);
  };
  return {
    time: (at: Date) => time.format(at),
    day,
    when: (at: Date, now: Date) => (key.format(at) === key.format(now) ? time.format(at) : `${date.format(at)} ${time.format(at)}`),
    groups: (snapshots: readonly Snapshot[], now: Date) => {
      const days: { day: string; snapshots: Snapshot[] }[] = [];
      for (const snapshot of snapshots) {
        const label = day(snapshot.at, now);
        const last = days.at(-1);
        if (last?.day === label) last.snapshots.push(snapshot);
        else days.push({ day: label, snapshots: [snapshot] });
      }
      return days;
    },
  };
}

export type SnapshotClock = ReturnType<typeof snapshotClock>;

export function sizeWords(bytes: number): string {
  const mb = bytes / 1e6;
  return mb >= 1000 ? `${(mb / 1000).toFixed(1)} GB` : `${Math.round(mb)} MB`;
}

export function spanWords(ms: number): string {
  const minutes = Math.max(1, Math.round(ms / 60_000));
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.round(minutes / 60);
  if (minutes < 1440) return hours === 1 ? "1 hour" : `${hours} hours`;
  const days = Math.round(minutes / 1440);
  return days === 1 ? "1 day" : `${days} days`;
}

/** What restoring `snapshot` drops: "1.2 GB · 3 hours of writes", or null when nothing was written since. */
export function lossWords(snapshot: Pick<Snapshot, "at" | "losesBytes">, now: Date): string | null {
  if (snapshot.losesBytes <= 0) return null;
  return `${sizeWords(snapshot.losesBytes)} · ${spanWords(now.getTime() - snapshot.at.getTime())} of writes`;
}

export function relWords(at: Date, now: Date): string {
  const ms = now.getTime() - at.getTime();
  return ms < 60_000 ? "just now" : `${spanWords(ms)} ago`;
}
