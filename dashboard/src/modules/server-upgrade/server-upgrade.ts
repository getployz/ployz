import { Schema } from "effect";
import type { ServerStatus } from "#/modules/machines/server-status";

/** How long Cloud waits for an Upgrade's outcome: longer than the upgrade worker's 15-minute cap. */
export const UPGRADE_OBSERVATION_LIMIT_MS = 20 * 60_000;

export const UPGRADE_OUTCOMES = ["running", "succeeded", "failed", "interrupted", "unknown"] as const;
export type UpgradeOutcome = (typeof UPGRADE_OUTCOMES)[number];
export const UPGRADE_TRIGGERS = ["automatic", "manual"] as const;
export type UpgradeTrigger = (typeof UPGRADE_TRIGGERS)[number];
export const RELEASE_CHANNELS = ["stable", "beta"] as const;
export type ReleaseChannel = (typeof RELEASE_CHANNELS)[number];

// The only forms a release is published in: `X.Y.Z` and `X.Y.Z-beta.N`.
const VERSION = /^(\d+)\.(\d+)\.(\d+)(?:-beta\.(\d+))?$/u;

function versionParts(version: string) {
  const match = VERSION.exec(version);
  if (match === null) return null;
  // A release sorts after every beta of it.
  return [match[1], match[2], match[3], match[4] ?? Infinity].map(Number);
}

/** Negative when `left` is older, zero when equal, positive when newer; null when either isn't a published version. */
export function compareVersions(left: string, right: string) {
  const a = versionParts(left);
  const b = versionParts(right);
  if (a === null || b === null) return null;
  const index = a.findIndex((part, at) => part !== b[at]);
  return index === -1 ? 0 : (a[index] ?? 0) - (b[index] ?? 0);
}

/** The release a Release Channel pointer names (`v0.2.2\n`), as Servers report versions (`0.2.2`). */
export function releaseFromPointer(text: string) {
  const version = text.trim().replace(/^v/u, "");
  return versionParts(version) === null ? null : version;
}

/** The release line a version belongs to, as the pointer path names it (`v0`). */
export function releaseLine(version: string) {
  const parts = versionParts(version);
  return parts === null ? null : `v${parts[0]}`;
}

export const ReleaseLine = Schema.String.check(Schema.isPattern(/^v\d{1,4}$/u));

export const RequestServerUpgradeInput = Schema.Struct({
  organizationSlug: Schema.String,
  /** Upgrade on a Server page names it; Upgrade on the Servers page upgrades every Server behind. */
  machineId: Schema.NullOr(Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/u))),
});
export type RequestServerUpgradeInput = typeof RequestServerUpgradeInput.Type;

export const SetAutomaticServerUpgradesInput = Schema.Struct({ organizationSlug: Schema.String, automatic: Schema.Boolean });
export type SetAutomaticServerUpgradesInput = typeof SetAutomaticServerUpgradesInput.Type;

/** The Org Store's view of the Organization's Server upgrade settings, keyed by Organization ID. */
export type ServerUpgradeSettingsRow = { readonly id: string; readonly automatic: boolean };

/** No row reads as the defaults: automatic upgrades on. */
export const automaticUpgrades = (rows: readonly ServerUpgradeSettingsRow[]) => rows[0]?.automatic ?? true;

/** A Server's latest Upgrade attempt, as the Server page reads it. */
export const LatestUpgrade = Schema.Struct({
  attemptId: Schema.String,
  outcome: Schema.Literals(UPGRADE_OUTCOMES),
  stage: Schema.NullOr(Schema.String),
  error: Schema.NullOr(Schema.String),
  fromVersion: Schema.String,
  targetVersion: Schema.NullOr(Schema.String),
  startedAt: Schema.String,
});
export type LatestUpgrade = typeof LatestUpgrade.Type;

export type ServerUpgradeLine =
  | { readonly kind: "upgrading"; readonly target: string | null }
  | {
    readonly kind: "failed";
    readonly target: string;
    /** True only while the Server is observed online on the version it ran before. */
    readonly nothingChanged: boolean;
    /** The exact error for a failure; the stage reached otherwise. */
    readonly details: string | null;
    readonly canRetry: boolean;
  }
  | { readonly kind: "behind"; readonly release: string }
  | null;

/**
 * The Server page's Upgrade line. `release` is the newest release on the Server's line; `pendingFrom` is the latest
 * attempt ID when the user clicked Upgrade (null with none), undefined when nothing is pending.
 */
export function serverUpgradeLine(input: {
  readonly version: string;
  readonly status: ServerStatus;
  readonly release: string | null;
  readonly latest: LatestUpgrade | null;
  readonly pendingFrom: string | null | undefined;
  readonly now: number;
}): ServerUpgradeLine {
  const { version, status, release, latest } = input;
  const pending = input.pendingFrom !== undefined && (latest?.attemptId ?? null) === input.pendingFrom;
  const expired = latest !== null && latest.outcome === "running"
    && input.now - Date.parse(latest.startedAt) >= UPGRADE_OBSERVATION_LIMIT_MS;
  const outcome = expired ? "unknown" : latest?.outcome;
  if (pending || outcome === "running") return { kind: "upgrading", target: latest?.targetVersion ?? release };

  const online = status === "online" || status === "building";
  const target = latest?.targetVersion ?? release;
  if (latest !== null && outcome !== "succeeded" && target !== null) {
    // A Server that has since reached the target, by any route, has nothing left to report.
    const passed = compareVersions(version, target);
    if (passed === null || passed < 0) {
      return {
        kind: "failed",
        target,
        nothingChanged: online && latest.fromVersion !== "" && version === latest.fromVersion,
        details: outcome === "failed" ? latest.error : latest.stage,
        canRetry: status === "online",
      };
    }
  }
  if (release === null || status !== "online") return null;
  const behind = compareVersions(version, release);
  return behind !== null && behind < 0 ? { kind: "behind", release } : null;
}

/** The attempt started last; null with none. */
export const newestAttempt = (latest: readonly LatestUpgrade[]) => latest.reduce<LatestUpgrade | null>((found, row) =>
  found === null || Date.parse(row.startedAt) > Date.parse(found.startedAt) ? row : found, null);

export type ServersUpgradeLine =
  | { readonly kind: "current"; readonly release: string; readonly upgradedAt: string | null }
  | { readonly kind: "upgrading"; readonly target: string; readonly done: number; readonly total: number }
  | {
    readonly kind: "behind";
    readonly release: string;
    readonly upgraded: number;
    readonly total: number;
    /** The oldest version the Servers behind report; null when none reports one. */
    readonly running: string | null;
  }
  | null;

/**
 * The Servers page's upgrade line. `latest` holds each Server's latest attempt; `lastUpgradedAt` ends the latest
 * successful one. `pendingFrom` is the newest attempt ID when the user clicked Upgrade (null with none), undefined
 * when nothing is pending.
 */
export function serversUpgradeLine(input: {
  readonly servers: ReadonlyArray<{ readonly version: string }>;
  readonly release: string | null;
  readonly latest: readonly LatestUpgrade[];
  readonly lastUpgradedAt: string | null;
  readonly pendingFrom: string | null | undefined;
  readonly now: number;
}): ServersUpgradeLine {
  const { servers, release, latest } = input;
  if (release === null || servers.length === 0) return null;
  const isCurrent = (version: string) => (compareVersions(version, release) ?? -1) >= 0;
  const behind = servers.filter(({ version }) => !isCurrent(version));
  const done = servers.length - behind.length;

  const pending = input.pendingFrom !== undefined && (newestAttempt(latest)?.attemptId ?? null) === input.pendingFrom;
  const running = latest.find((row) =>
    row.outcome === "running" && input.now - Date.parse(row.startedAt) < UPGRADE_OBSERVATION_LIMIT_MS);
  if (pending || running !== undefined) {
    return { kind: "upgrading", target: running?.targetVersion ?? release, done, total: servers.length };
  }

  if (behind.length === 0) return { kind: "current", release, upgradedAt: input.lastUpgradedAt };
  const oldest = behind.map(({ version }) => version).filter((version) => compareVersions(version, release) !== null)
    .sort((left, right) => compareVersions(left, right) ?? 0)[0] ?? null;
  return { kind: "behind", release, upgraded: done, total: servers.length, running: oldest };
}
