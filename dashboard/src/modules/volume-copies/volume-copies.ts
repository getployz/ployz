import type { MachineId, MachineName } from "@ployz/sdk";
import { Schema } from "effect";
import type { Heard } from "#/modules/volume-run/volume-run";
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
  /** Why the last refresh did not land, so the age keeps growing until it catches up. */
  readonly skipped: Exclude<Heard, "answered"> | "failed" | null;
  /** Known only from its Server's last report: that Server isn't answering and no Deploy recorded the mirror. */
  readonly lastReported: boolean;
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

/**
 * What the primary's Server said of it: `live` on its own report of a writable root, `read_only` when the root it reports
 * is read-only, as a cut leaves it, else what was heard instead of a report.
 */
export type LiveState = "live" | "read_only" | Exclude<Heard, "answered">;

/** The primary's Server: the one that reports it, else the one the applied placement names, which hasn't. */
export type PrimaryServer = ServerRef & { readonly state: LiveState };

export type LiveCopy = {
  readonly server: PrimaryServer | null;
  readonly restore: { readonly label: string } | null;
  readonly moveTo: ServerRef | null;
  readonly busy: string | null;
  /** Null where the Server does not serve snapshots. */
  readonly snapshots: readonly Snapshot[] | null;
};

/** A Server outside Live that didn't report its copy, and what was heard from it instead. */
export type UnheardServer = ServerRef & { readonly heard: Exclude<Heard, "answered"> };

export type VolumeCopies = {
  readonly volume: { readonly id: string; readonly name: string };
  readonly live: LiveCopy;
  readonly elsewhere: readonly VolumeCopy[];
  /** While any is listed, what is outside Live isn't known. */
  readonly unheard: readonly UnheardServer[];
};

export const STALE_AFTER_MS = 60 * 60_000;

export const COPY_WORDS = { mirror: "Mirror", offsite: "Offsite", old: "Old copy" } as const satisfies Record<VolumeCopy["kind"], string>;
export const STAGED_WORDS = { new: "New", removing: "Removing", primary: "Primary" } as const satisfies Record<StagedCopy, string>;
/** What a Server said instead of reporting its copy, on the primary's badge and a mirror's row alike. */
const UNHEARD_WORDS = {
  outdated: "Needs a newer Ployz", lacks: "Can't report this copy", silent: "Not answering", waiting: "Waiting for its report",
} as const satisfies Record<Exclude<Heard, "answered">, string>;
/**
 * The primary's badge: Live only on its Server's own report of a writable root. Not answering names no time, since nothing
 * says when it stopped. Needs a newer Ployz only once its release is known to be behind.
 */
export const LIVE_WORDS = { live: "Live", read_only: "Read-only", ...UNHEARD_WORDS } as const satisfies Record<LiveState, string>;

const UNHEARD_CLAUSES = {
  outdated: (name) => `${name} needs a newer Ployz`,
  lacks: (name) => `${name} can't report this copy`,
  silent: (name) => `${name} is not answering`,
  waiting: (name) => `${name}'s report hasn't arrived`,
} as const satisfies Record<Exclude<Heard, "answered">, (name: string) => string>;

/** Elsewhere's line when it lists no copy: that nothing is outside Live, or which Servers keep Copies from knowing. */
export function nothingElsewhere(copies: VolumeCopies): string {
  const live = copies.live.server?.name ?? "its server";
  if (copies.unheard.length === 0) return `Nothing outside ${live}. If ${live} dies, ${copies.volume.name} goes with it.`;
  return `${copies.unheard.map((server) => UNHEARD_CLAUSES[server.heard](server.name)).join(" and ")}, so Copies can't say what is outside ${live}.`;
}

/** Why a mirror's Server didn't report it just now, said beside its age however fresh: the age is only its last match. */
export function unheardReason(copy: VolumeCopy): string | null {
  if (copy.kind !== "mirror" || copy.busy !== null || copy.skipped === null || copy.skipped === "failed") return null;
  return UNHEARD_WORDS[copy.skipped];
}

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

/** A mirror that has never refreshed protects nothing yet, so it reads as behind. */
export function isStale(copy: VolumeCopy, now: Date): boolean {
  return copy.kind === "mirror" && copy.staged !== "new"
    && (copy.refreshedAt === null || now.getTime() - copy.refreshedAt.getTime() > STALE_AFTER_MS);
}

export const hangingCopies = (copies: VolumeCopies): readonly (MirrorCopy | OffsiteCopy)[] =>
  copies.elsewhere.filter((copy): copy is MirrorCopy | OffsiteCopy => copy.kind !== "old");

export function addableCopies(copies: VolumeCopies, served: readonly AddableCopy[]): readonly AddableCopy[] {
  return served.filter((kind) => !copies.elsewhere.some((copy) => copy.kind === kind));
}

export type CardLine = { readonly spin: boolean; readonly words: string; readonly age: string | null; readonly stale: boolean };

/** A staged removal drops the kind word so the Removing badge reads alone. */
export function cardLine(copy: VolumeCopy, now: Date): CardLine {
  if (copy.busy !== null) return { spin: true, words: copy.busy, age: null, stale: false };
  if (copy.staged !== null) return { spin: false, words: copy.staged === "removing" ? "" : COPY_WORDS[copy.kind], age: null, stale: false };
  const at = lastAt(copy);
  const stale = isStale(copy, now);
  const words = copy.kind === "mirror" && copy.lastReported ? `${COPY_WORDS.mirror} · last reported by ${copy.server.name}` : COPY_WORDS[copy.kind];
  return { spin: false, words, age: at === null ? (stale ? "Not yet" : null) : age(at, now), stale };
}

export type TraySpeech = { readonly tone: "busy" | "staged"; readonly words: string };

export function traySpeech(copies: VolumeCopies): TraySpeech | null {
  const { live } = copies;
  if (live.busy !== null) return { tone: "busy", words: live.busy };
  if (live.restore !== null) return { tone: "staged", words: `Restore ${live.restore.label}` };
  if (live.moveTo !== null) return { tone: "staged", words: `Moving to ${live.moveTo.name}` };
  return null;
}

/** Sync now: one incremental send to a Volume's mirror. */
export const RefreshMirrorInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  volumeId: Schema.NonEmptyString,
  environment: Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) }),
});
