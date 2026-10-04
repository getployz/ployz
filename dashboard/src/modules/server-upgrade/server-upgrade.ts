import { Schema } from "effect";
import { machineIdStringSchema } from "#/modules/machines/enrollment";
import type { ServerStatus } from "#/modules/machines/server-status";

/** How long Cloud waits for an Upgrade's outcome: longer than the upgrade worker's 15-minute cap. */
export const UPGRADE_OBSERVATION_LIMIT_MS = 20 * 60_000;

export const UPGRADE_OUTCOMES = ["running", "succeeded", "failed", "interrupted", "unknown"] as const;
export type UpgradeOutcome = (typeof UPGRADE_OUTCOMES)[number];
export const UPGRADE_TRIGGERS = ["automatic", "manual"] as const;
export type UpgradeTrigger = (typeof UPGRADE_TRIGGERS)[number];
export const RELEASE_CHANNELS = ["stable", "beta"] as const;
export type ReleaseChannel = (typeof RELEASE_CHANNELS)[number];
export type FinalOutcome = Exclude<UpgradeOutcome, "running">;

/** The settings of an Organization without a settings row: automatic upgrades on, Stable releases. */
export const DEFAULT_SERVER_UPGRADE_SETTINGS: { readonly automatic: boolean; readonly channel: ReleaseChannel } = {
  automatic: true,
  channel: "stable",
};

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

/** A Server runs a published version older than `release`. An unknown version is never behind: Cloud can't upgrade it. */
export function isBehind(version: string, release: string | null) {
  const order = release === null ? null : compareVersions(version, release);
  return order !== null && order < 0;
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

/**
 * The new-major-line notice: the release the unscoped `stable` pointer names, when its line is newer than the Servers'
 * `line` (`v0`). `name` reads it as its line (`1.0`), `running` the Servers' line (`0.x`).
 */
export function newMajorLine(line: string | null, newest: string | null) {
  const [major, minor] = newest === null ? [] : versionParts(newest) ?? [];
  if (line === null || newest === null || major === undefined || major <= Number(line.slice(1))) return null;
  return { release: newest, name: `${major}.${minor}`, running: `${line.slice(1)}.x` };
}

export const RequestServerUpgradeInput = Schema.Struct({
  organizationSlug: Schema.String,
  /** Upgrade on a Server page names it; Upgrade on the Servers page upgrades every Server behind. */
  machineId: Schema.NullOr(machineIdStringSchema),
});
export type RequestServerUpgradeInput = typeof RequestServerUpgradeInput.Type;

/** The Server upgrades dialog changes one setting at a time; any it leaves out keeps its value. */
export const SetServerUpgradeSettingsInput = Schema.Struct({
  organizationSlug: Schema.String,
  automatic: Schema.optionalKey(Schema.Boolean),
  channel: Schema.optionalKey(Schema.Literals(RELEASE_CHANNELS)),
});
export type SetServerUpgradeSettingsInput = typeof SetServerUpgradeSettingsInput.Type;

/** The Org Store's view of the Organization's Server upgrade settings, keyed by Organization ID. */
export type ServerUpgradeSettingsRow = { readonly id: string; readonly automatic: boolean; readonly channel: ReleaseChannel };

/** No row reads as the defaults. */
export const serverUpgradeSettings = (rows: readonly ServerUpgradeSettingsRow[]) => ({
  automatic: rows[0]?.automatic ?? DEFAULT_SERVER_UPGRADE_SETTINGS.automatic,
  channel: rows[0]?.channel ?? DEFAULT_SERVER_UPGRADE_SETTINGS.channel,
});

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
  | { readonly kind: "behind"; readonly release: string; readonly canUpgrade: boolean }
  /** Offline and behind while automatic upgrades are on: a later check picks it up. */
  | { readonly kind: "when-back"; readonly release: string }
  | null;

/**
 * What an attempt that ended keeps besides its outcome: nothing after a success, the stage a non-success stopped at,
 * and a failure's error too. The row, its PostHog event, and the Server page's details all read it.
 */
export function endEvidence(outcome: FinalOutcome, stage: string | null, error: string | null): {
  readonly stage?: string | null;
  readonly error?: string | null;
} {
  if (outcome === "succeeded") return {};
  return outcome === "failed" ? { stage, error } : { stage };
}

/** An attempt started at `startedAt` has outlived the observation limit: Cloud stops waiting and it reads as unknown. */
export const outlivedObservation = (startedAt: Date, now: number) => now - startedAt.getTime() >= UPGRADE_OBSERVATION_LIMIT_MS;

/** An attempt still within the observation limit is running, and so is the Organization's one Rollout. */
const isRunning = (row: LatestUpgrade, now: number) =>
  row.outcome === "running" && !outlivedObservation(new Date(row.startedAt), now);

/** The newest attempt ID when the user clicked Upgrade (null with none); undefined when nothing is pending. */
export type PendingFrom = string | null | undefined;

/** Whether any Server's latest attempt is running: the Organization's Rollout is under way. */
export const rolloutRunning = (latest: readonly LatestUpgrade[], now: number) => latest.some((row) => isRunning(row, now));

/**
 * The Server page's Upgrade line. `release` is the newest release on the Server's line. Upgrade and Try again wait
 * while `rolloutRunning`.
 */
export function serverUpgradeLine(input: {
  readonly version: string;
  readonly status: ServerStatus;
  readonly release: string | null;
  readonly latest: LatestUpgrade | null;
  readonly pendingFrom: PendingFrom;
  readonly now: number;
  readonly automatic: boolean;
  readonly rolloutRunning: boolean;
}): ServerUpgradeLine {
  const { version, status, release, latest } = input;
  const pending = input.pendingFrom !== undefined && (latest?.attemptId ?? null) === input.pendingFrom;
  const expired = latest !== null && latest.outcome === "running" && !isRunning(latest, input.now);
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
        details: details(endEvidence(outcome ?? "unknown", latest.stage, latest.error)),
        canRetry: status === "online" && !input.rolloutRunning,
      };
    }
  }
  if (release === null || !isBehind(version, release)) return null;
  if (status === "online") return { kind: "behind", release, canUpgrade: !input.rolloutRunning };
  return status === "offline" && input.automatic ? { kind: "when-back", release } : null;
}

/** The exact error for a failure; the stage reached otherwise. */
const details = (evidence: ReturnType<typeof endEvidence>) => evidence.error ?? evidence.stage ?? null;

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
    /** The oldest version the Servers behind report. */
    readonly running: string | null;
    /** Some Server behind is online and idle, so an Upgrade has one to take. */
    readonly canUpgrade: boolean;
  }
  /** Only offline Servers are behind, and automatic upgrades pick them up once they're back. */
  | { readonly kind: "when-back"; readonly release: string; readonly names: readonly string[] }
  | null;

/**
 * The Servers page's upgrade line. `latest` holds each Server's latest attempt; `lastUpgradedAt` ends the latest
 * successful one. A Server whose version is unknown is neither behind nor upgraded.
 */
export function serversUpgradeLine(input: {
  readonly servers: ReadonlyArray<{ readonly name: string; readonly version: string; readonly status: ServerStatus }>;
  readonly release: string | null;
  readonly latest: readonly LatestUpgrade[];
  readonly lastUpgradedAt: string | null;
  readonly pendingFrom: PendingFrom;
  readonly now: number;
  readonly automatic: boolean;
}): ServersUpgradeLine {
  const { servers, release, latest } = input;
  if (release === null || servers.length === 0) return null;
  const behind = servers.filter(({ version }) => isBehind(version, release));
  const done = servers.filter(({ version }) => (compareVersions(version, release) ?? -1) >= 0).length;

  const pending = input.pendingFrom !== undefined && (newestAttempt(latest)?.attemptId ?? null) === input.pendingFrom;
  const running = latest.find((row) => isRunning(row, input.now));
  if (pending || running !== undefined) {
    return { kind: "upgrading", target: running?.targetVersion ?? release, done, total: servers.length };
  }

  if (behind.length === 0) return done === servers.length ? { kind: "current", release, upgradedAt: input.lastUpgradedAt } : null;
  if (input.automatic && behind.every(({ status }) => status === "offline")) {
    return { kind: "when-back", release, names: behind.map(({ name }) => name) };
  }
  const oldest = behind.map(({ version }) => version).sort((left, right) => compareVersions(left, right) ?? 0)[0] ?? null;
  return {
    kind: "behind",
    release,
    upgraded: done,
    total: servers.length,
    running: oldest,
    canUpgrade: behind.some(({ status }) => status === "online"),
  };
}
