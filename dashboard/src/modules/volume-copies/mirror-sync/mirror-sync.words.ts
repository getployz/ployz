import { copyAge, isStale, type MirrorCopy } from "#/modules/volume-copies/volume-copies";

/** The cron's cadence: a mirror round every 15 minutes. */
export const MIRROR_ROUND_MINUTES = 15;

export type MirrorLast = { readonly spin: boolean; readonly words: string; readonly stale: boolean };

/** The mirror's Last line: what runs on it now, else how old its newest point is. */
export function mirrorLast(copy: MirrorCopy, now: Date): MirrorLast {
  if (copy.busy !== null) return { spin: true, words: copy.busy, stale: false };
  const at = copy.staged === "new" ? null : copy.refreshedAt;
  return { spin: false, words: copyAge(at, now), stale: isStale(copy, now) };
}

/**
 * Why a mirror is behind, or null while its rounds land. Said only once the mirror is behind, with what a failover loses.
 */
export function skippedWords(copy: MirrorCopy, now: Date): string | null {
  if (!copy.failed || copy.busy !== null || copy.refreshedAt === null || !isStale(copy, now)) return null;
  const behind = span(Math.round((now.getTime() - copy.refreshedAt.getTime()) / 60_000));
  return `The last refresh failed. Ployz retries every ${MIRROR_ROUND_MINUTES} minutes. Until it catches up, a failover loses ${behind} of writes.`;
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
