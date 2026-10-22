import { formatRelativeTime } from "#/utils/relative-time";
import { isStale, type MirrorCopy } from "./volume-copies";

/** The cron's cadence: a mirror round every 15 minutes. */
export const MIRROR_ROUND_MINUTES = 15;

export type MirrorLast = { readonly spin: boolean; readonly words: string; readonly stale: boolean };

/** The mirror's Last line: what runs on it now, else how old its newest point is. */
export function mirrorLast(copy: MirrorCopy, now: Date): MirrorLast {
  if (copy.busy !== null) return { spin: true, words: copy.busy, stale: false };
  const at = copy.staged === "new" ? null : copy.refreshedAt;
  if (at === null) return { spin: false, words: "Not yet", stale: isStale(copy, now) };
  const words = now.getTime() - at.getTime() < 60_000 ? "just now" : formatRelativeTime(at, now);
  return { spin: false, words, stale: isStale(copy, now) };
}

const SKIPPED_WORDS = {
  outdated: (server) => `${server} needs a newer Ployz.`,
  lacks: (server) => `${server} can't report this copy.`,
  silent: (server) => `${server} is not answering.`,
  waiting: () => "Waiting for its server's report.",
  failed: () => "The last refresh failed.",
} as const satisfies Record<NonNullable<MirrorCopy["skipped"]>, (server: string) => string>;

/**
 * Why a mirror is behind, or null while its rounds land. Names no time: a round skips a Volume whose Server is silent or
 * that a Deploy holds, so the last good round says nothing about when the Server stopped answering. A Server that didn't
 * report the mirror is said at once; what a failover loses is said only once the mirror is behind.
 */
export function skippedWords(copy: MirrorCopy, now: Date): string | null {
  if (copy.skipped === null || copy.busy !== null) return null;
  const cause = SKIPPED_WORDS[copy.skipped](copy.server.name);
  if (copy.refreshedAt === null || !isStale(copy, now)) return copy.skipped === "failed" ? null : cause;
  const behind = span(Math.round((now.getTime() - copy.refreshedAt.getTime()) / 60_000));
  return `${cause} Ployz retries every ${MIRROR_ROUND_MINUTES} minutes. Until it catches up, a failover loses ${behind} of writes.`;
}

function span(minutes: number) {
  if (minutes < 60) return `${minutes} min`;
  if (minutes < 1440) {
    const hours = Math.round(minutes / 60);
    return hours === 1 ? "1 hour" : `${hours} hours`;
  }
  const days = Math.round(minutes / 1440);
  return days === 1 ? "1 day" : `${days} days`;
}
