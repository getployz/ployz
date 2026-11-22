import type { MachineId, MachineName } from "@ployz/sdk";
import { type Heard, STALE_AFTER_MS } from "#/modules/volume-run/volume-run";
import { formatRelativeTime } from "#/utils/relative-time";

export type ServerRef = { readonly id: MachineId; readonly name: MachineName };

export type StagedCopy = "new" | "removing" | "primary";

type CopyBase = {
  /** Stable within the Volume: the Server id. */
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
  readonly skipped: Unheard | "failed" | null;
  /**
   * Known only from its silent Server's last report, as no placement holds it: shown, never staged or recovered from.
   * Nothing says when that report was, so it has no age and is never behind.
   */
  readonly lastReported: boolean;
};

export type OldCopy = CopyBase & {
  readonly kind: "old";
  readonly server: ServerRef;
  readonly name: string;
  readonly since: Date | null;
  readonly bytes: number | null;
};

export type VolumeCopy = MirrorCopy | OldCopy;

/**
 * What the primary's Server said of it: `live` on its own report of a writable root, `read_only` when the root it reports
 * is read-only, as a cut leaves it, else what was heard instead of a report.
 */
export type LiveState = "live" | "read_only" | Unheard;

/** The primary's Server: the one that reports it, else the one the applied placement names, which hasn't. */
export type PrimaryServer = ServerRef & { readonly state: LiveState };

export type LiveCopy = {
  readonly server: PrimaryServer | null;
  readonly restore: { readonly label: string } | null;
  readonly moveTo: ServerRef | null;
  readonly busy: string | null;
};

/** A Server outside Live that didn't report its copy, and what was heard from it instead. */
export type UnheardServer = ServerRef & { readonly heard: Unheard };

export type VolumeCopies = {
  readonly volume: { readonly id: string; readonly name: string };
  readonly live: LiveCopy;
  readonly elsewhere: readonly VolumeCopy[];
  /** While any is listed, what is outside Live isn't known. */
  readonly unheard: readonly UnheardServer[];
};

export const COPY_WORDS = { mirror: "Mirror", old: "Old copy" } as const satisfies Record<VolumeCopy["kind"], string>;
export const STAGED_WORDS = { new: "New", removing: "Removing", primary: "Primary" } as const satisfies Record<StagedCopy, string>;
type Unheard = Exclude<Heard, "answered">;

/**
 * What a Server said instead of reporting its copy, worded for each surface: `badge` on the primary's badge and a mirror's
 * row, `clause` inside Elsewhere's line, `sentence` in the mirror's Behind explanation.
 */
export const HEARD_WORDS = {
  outdated: { badge: "Needs a newer Ployz", clause: (name) => `${name} needs a newer Ployz`, sentence: (name) => `${name} needs a newer Ployz.` },
  lacks: { badge: "Can't report this copy", clause: (name) => `${name} can't report this copy`, sentence: (name) => `${name} can't report this copy.` },
  silent: { badge: "Not answering", clause: (name) => `${name} is not answering`, sentence: (name) => `${name} is not answering.` },
  waiting: { badge: "Waiting for its report", clause: (name) => `${name}'s report hasn't arrived`, sentence: () => "Waiting for its server's report." },
} as const satisfies Record<Unheard, { readonly badge: string; readonly clause: (name: string) => string; readonly sentence: (name: string) => string }>;

/**
 * The primary's badge: Live only on its Server's own report of a writable root. Not answering names no time, since nothing
 * says when it stopped. Needs a newer Ployz only once its release is known to be behind.
 */
export const LIVE_WORDS = {
  live: "Live", read_only: "Read-only",
  outdated: HEARD_WORDS.outdated.badge, lacks: HEARD_WORDS.lacks.badge, silent: HEARD_WORDS.silent.badge, waiting: HEARD_WORDS.waiting.badge,
} as const satisfies Record<LiveState, string>;

/** Elsewhere's line when it lists no copy: that nothing is outside Live, or which Servers keep Copies from knowing. */
export function nothingElsewhere(copies: VolumeCopies): string {
  const live = copies.live.server?.name ?? "its server";
  if (copies.unheard.length === 0) return `Nothing outside ${live}. If ${live} dies, ${copies.volume.name} goes with it.`;
  return `${copies.unheard.map((server) => HEARD_WORDS[server.heard].clause(server.name)).join(" and ")}, so Copies can't say what is outside ${live}.`;
}

/** Why a mirror's Server didn't report it just now, said beside its age however fresh: the age is only its last match. */
export function unheardReason(copy: VolumeCopy): string | null {
  if (copy.kind !== "mirror" || copy.busy !== null || copy.skipped === null || copy.skipped === "failed") return null;
  return HEARD_WORDS[copy.skipped].badge;
}

/** How long ago a copy's newest point is; "Not yet" for one with none. */
export function copyAge(at: Date | null, now: Date): string {
  if (at === null) return "Not yet";
  return now.getTime() - at.getTime() < 60_000 ? "just now" : formatRelativeTime(at, now);
}

export function lastAt(copy: VolumeCopy): Date | null {
  switch (copy.kind) {
    case "mirror":
      return copy.refreshedAt;
    case "old":
      return copy.since;
  }
}

/** A mirror that has never refreshed protects nothing yet, so it reads as behind. */
export function isStale(copy: VolumeCopy, now: Date): boolean {
  return copy.kind === "mirror" && copy.staged !== "new" && !copy.lastReported
    && (copy.refreshedAt === null || now.getTime() - copy.refreshedAt.getTime() > STALE_AFTER_MS);
}

export const hangingCopies = (copies: VolumeCopies): readonly MirrorCopy[] =>
  copies.elsewhere.filter((copy): copy is MirrorCopy => copy.kind === "mirror");

export type CardLine = { readonly spin: boolean; readonly words: string; readonly age: string | null; readonly stale: boolean };

/** What a mirror known only from its silent Server's last report says in place of its kind: it may be gone since. */
export const LAST_REPORTED = "Last reported";

/**
 * A staged removal drops the kind word so the Removing badge reads alone. A mirror known only from its Server's last
 * report reads as last reported, staged or not, never as a mirror held now.
 */
export function cardLine(copy: VolumeCopy, now: Date): CardLine {
  if (copy.busy !== null) return { spin: true, words: copy.busy, age: null, stale: false };
  const words = copy.kind === "mirror" && copy.lastReported ? LAST_REPORTED : COPY_WORDS[copy.kind];
  if (copy.staged !== null) return { spin: false, words: copy.staged === "removing" ? "" : words, age: null, stale: false };
  const at = lastAt(copy);
  const stale = isStale(copy, now);
  return { spin: false, words, age: at === null && !stale ? null : copyAge(at, now), stale };
}

export type TraySpeech = { readonly tone: "busy" | "staged"; readonly words: string };

export function traySpeech(copies: VolumeCopies): TraySpeech | null {
  const { live } = copies;
  if (live.busy !== null) return { tone: "busy", words: live.busy };
  if (live.restore !== null) return { tone: "staged", words: `Restore ${live.restore.label}` };
  if (live.moveTo !== null) return { tone: "staged", words: `Moving to ${live.moveTo.name}` };
  return null;
}

