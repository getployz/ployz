import type { MachineId, MachineName } from "@ployz/sdk";
import { formatRelativeTime } from "#/utils/relative-time";

export type ServerRef = { readonly id: MachineId; readonly name: MachineName };

export type StagedCopy = "new" | "removing" | "primary";

type CopyBase = {
  /** Stable within the Volume: the Server id for a mirror or old copy, the bucket for an offsite copy. */
  readonly id: string;
  readonly staged: StagedCopy | null;
  readonly busy: string | null;
};

export type MirrorCopy = CopyBase & {
  readonly kind: "mirror";
  readonly server: ServerRef;
  readonly name: string;
  readonly refreshedAt: Date | null;
  /** The last refresh did not run, so the age keeps growing until it catches up. */
  readonly skipped: boolean;
};

export type OffsiteCopy = CopyBase & {
  readonly kind: "offsite";
  readonly bucket: string;
  readonly region: string;
  readonly keep: number;
  readonly uploadedAt: Date | null;
};

export type OldCopy = CopyBase & {
  readonly kind: "old";
  readonly server: ServerRef;
  readonly name: string;
  readonly since: Date | null;
  readonly bytes: number | null;
};

export type VolumeCopy = MirrorCopy | OffsiteCopy | OldCopy;
export type AddableCopy = Exclude<VolumeCopy["kind"], "old">;

export type SnapshotKind = "hourly" | "daily" | "manual" | "before_deploy" | "after_deploy" | "before_restore";

export type Snapshot = {
  readonly id: string;
  readonly at: Date;
  readonly kind: SnapshotKind;
  readonly note: string | null;
  readonly kept: boolean;
  readonly bytes: number;
};

export type LiveCopy = {
  readonly server: ServerRef | null;
  readonly restore: { readonly label: string } | null;
  readonly moveTo: ServerRef | null;
  readonly busy: string | null;
  /** Null where the Server does not serve snapshots. */
  readonly snapshots: readonly Snapshot[] | null;
};

export type VolumeCopies = {
  readonly volume: { readonly id: string; readonly name: string };
  readonly live: LiveCopy;
  readonly elsewhere: readonly VolumeCopy[];
};

export const STALE_AFTER_MS = 60 * 60_000;

export const COPY_WORDS = { mirror: "Mirror", offsite: "Offsite", old: "Old copy" } as const satisfies Record<VolumeCopy["kind"], string>;
export const STAGED_WORDS = { new: "New", removing: "Removing", primary: "Primary" } as const satisfies Record<StagedCopy, string>;

const age = (at: Date, now: Date) => (now.getTime() - at.getTime() < 60_000 ? "just now" : formatRelativeTime(at, now));

export function lastAt(copy: VolumeCopy): Date | null {
  switch (copy.kind) {
    case "mirror":
      return copy.refreshedAt;
    case "offsite":
      return copy.uploadedAt;
    case "old":
      return copy.since;
  }
}

export function isStale(copy: VolumeCopy, now: Date): boolean {
  return copy.kind === "mirror" && copy.staged !== "new" && copy.refreshedAt !== null
    && now.getTime() - copy.refreshedAt.getTime() > STALE_AFTER_MS;
}

export const hangingCopies = (copies: VolumeCopies): readonly (MirrorCopy | OffsiteCopy)[] =>
  copies.elsewhere.filter((copy): copy is MirrorCopy | OffsiteCopy => copy.kind !== "old");

export function addableCopies(copies: VolumeCopies, served: readonly AddableCopy[]): readonly AddableCopy[] {
  return served.filter((kind) => !copies.elsewhere.some((copy) => copy.kind === kind));
}

export type SafetyLine = { readonly tone: "ok" | "warn"; readonly words: string; readonly detail: string | null };

export function safetyLine(copies: VolumeCopies, now: Date): SafetyLine {
  const places = hangingCopies(copies).filter((copy) => copy.staged !== "new");
  const snapshots = copies.live.snapshots;
  if (places.length === 0) {
    const on = copies.live.server === null ? "" : ` on ${copies.live.server.name}`;
    return { tone: "warn", words: "No copies", detail: snapshots === null ? null : `${count(snapshots.length, "snapshot")}${on}` };
  }
  const mirror = places.find((copy) => copy.kind === "mirror");
  if (mirror !== undefined && isStale(mirror, now) && mirror.refreshedAt !== null) {
    return { tone: "warn", words: "Mirror behind", detail: age(mirror.refreshedAt, now) };
  }
  const words = [count(places.length, "copy", "copies"), snapshots === null ? null : count(snapshots.length, "snapshot")];
  return { tone: "ok", words: words.filter(Boolean).join(" · "), detail: null };
}

export type CardLine = { readonly spin: boolean; readonly words: string; readonly age: string | null; readonly stale: boolean };

/** A staged removal drops the kind word so the Removing badge reads alone. */
export function cardLine(copy: VolumeCopy, now: Date): CardLine {
  if (copy.busy !== null) return { spin: true, words: copy.busy, age: null, stale: false };
  if (copy.staged !== null) return { spin: false, words: copy.staged === "removing" ? "" : COPY_WORDS[copy.kind], age: null, stale: false };
  const at = lastAt(copy);
  return { spin: false, words: COPY_WORDS[copy.kind], age: at === null ? null : age(at, now), stale: isStale(copy, now) };
}

export type TraySpeech = { readonly tone: "busy" | "staged"; readonly words: string };

export function traySpeech(copies: VolumeCopies): TraySpeech | null {
  const { live } = copies;
  if (live.busy !== null) return { tone: "busy", words: live.busy };
  if (live.restore !== null) return { tone: "staged", words: `Restore ${live.restore.label}` };
  if (live.moveTo !== null) return { tone: "staged", words: `Moving to ${live.moveTo.name}` };
  return null;
}

function count(n: number, one: string, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}
