import type { Snapshot } from "./volume-copies";
import { spanWords } from "./volume-snapshots";

export type SnapshotEvery = "hour" | "day" | "off";

/** A Volume's automatic snapshots: the deploy toggles and their keep count, one schedule, and a space cap. */
export type SnapshotPolicy = {
  readonly before: boolean;
  readonly after: boolean;
  readonly keepDeploys: number;
  readonly every: SnapshotEvery;
  readonly keep: number;
  readonly capGb: number;
};

export type PolicyCount = "keepDeploys" | "keep" | "capGb";

export const POLICY_LIMITS = { keepDeploys: [1, 50], keep: [1, 168], capGb: [1, 20] } as const satisfies Record<PolicyCount, readonly [number, number]>;

export const DEFAULT_POLICY: SnapshotPolicy = { before: false, after: false, keepDeploys: 5, every: "off", keep: 24, capGb: 10 };

export const defaultKeep = (every: Exclude<SnapshotEvery, "off">) => (every === "day" ? 7 : 24);

export const clampCount = (count: PolicyCount, value: number) => {
  const [low, high] = POLICY_LIMITS[count];
  return Math.max(low, Math.min(high, value));
};

const SCHEDULED: readonly Snapshot["kind"][] = ["hourly", "daily"];
const DEPLOYS: readonly Snapshot["kind"][] = ["before_deploy", "after_deploy"];

/**
 * The automatic snapshots `policy` drops, oldest last in `snapshots` (newest first): scheduled ones past `keep`,
 * deploy ones past `keepDeploys`, then the oldest automatic ones until the rest fit under the cap. Kept and manual
 * snapshots always stay.
 */
export function pruneIds(snapshots: readonly Pick<Snapshot, "id" | "kind" | "kept" | "bytes">[], policy: SnapshotPolicy): readonly string[] {
  const auto = (kinds: readonly Snapshot["kind"][]) => snapshots.filter((snapshot) => !snapshot.kept && kinds.includes(snapshot.kind));
  const drop = new Set<string>();
  if (policy.every !== "off") for (const snapshot of auto(SCHEDULED).slice(policy.keep)) drop.add(snapshot.id);
  for (const snapshot of auto(DEPLOYS).slice(policy.keepDeploys)) drop.add(snapshot.id);
  let total = snapshots.filter((snapshot) => !drop.has(snapshot.id)).reduce((sum, snapshot) => sum + snapshot.bytes, 0);
  const oldestFirst = auto([...SCHEDULED, ...DEPLOYS]).filter((snapshot) => !drop.has(snapshot.id)).reverse();
  for (const snapshot of oldestFirst) {
    if (total <= policy.capGb * 1e9) break;
    drop.add(snapshot.id);
    total -= snapshot.bytes;
  }
  return snapshots.filter((snapshot) => drop.has(snapshot.id)).map((snapshot) => snapshot.id);
}

/** A short stable hash in [a-z0-9], so minted ids stay inside the wire's 64 characters whatever the source id holds. */
function digest(text: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    h1 = Math.imul(h1 ^ code, 2654435761);
    h2 = Math.imul(h2 ^ code, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(36);
}

export const HOUR_MS = 3_600_000;

/** The hour `now` falls in; every scheduled run names its tick, so a retry takes the same snapshot. */
export const tickOf = (now: Date) => new Date(Math.floor(now.getTime() / HOUR_MS) * HOUR_MS);

/** Hourly snapshots run every tick; daily ones run on the midnight UTC tick. */
export const dueAt = (every: SnapshotEvery, tick: Date) => every === "hour" || (every === "day" && tick.getUTCHours() === 0);

export const scheduledSnapshotId = (every: Exclude<SnapshotEvery, "off">, volumeId: string, tick: Date) =>
  `${every === "hour" ? "hourly" : "daily"}-${Math.floor(tick.getTime() / 1000)}-${digest(volumeId)}`;

export type DeployMoment = "before" | "after";

export const deploySnapshotId = (moment: DeployMoment, deploymentId: string) => `${moment}-${digest(deploymentId)}`;

export function deploysWords(policy: SnapshotPolicy): string {
  if (!policy.before && !policy.after) return "Off";
  const which = policy.before && policy.after ? "Before and after" : policy.before ? "Before" : "After";
  return `${which} · ${policy.keepDeploys} kept`;
}

export const scheduleWords = (policy: SnapshotPolicy) =>
  policy.every === "off" ? "Off" : `${policy.every === "hour" ? "Hourly" : "Daily"} · ${policy.keep} kept`;

/** How far back `keep` scheduled snapshots reach: "1 day" for 24 hourly ones. */
export const keepSpanWords = (policy: SnapshotPolicy) => spanWords(policy.keep * (policy.every === "day" ? 24 : 1) * HOUR_MS);
