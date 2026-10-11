import { Schema } from "effect";
import { RangeEdge } from "./uptime-range";

const Int = Schema.Finite.check(Schema.isInt());

/**
 * One probe the detector judged on, as `observation_json` in uptime/src/control/incidents.rs writes it. `status` is 0
 * when there was no HTTP response. The Rust test and uptime-status.postgres.test.ts both hold
 * incident-evidence.fixture.json, so the two sides can't drift apart.
 */
export const IncidentObservation = Schema.Struct({
  region: Schema.String,
  kind: Schema.Literals(["routine", "confirm", "recheck"]),
  verdict: Schema.Literals(["pass", "fail", "inconclusive"]),
  status: Int,
  failedPhase: Schema.NullOr(Schema.Literals(["dns", "connect", "tls", "request"])),
  error: Schema.NullOr(Schema.Literals(["connection_refused", "timed_out", "other"])),
  remoteIp: Schema.NullOr(Schema.String),
  at: Schema.String,
});
export type IncidentObservation = typeof IncidentObservation.Type;

/** uptime_incident.opened_by, as `evidence_json` in uptime/src/control/incidents.rs writes it. */
export const IncidentEvidence = Schema.Struct({
  quorum: Schema.Union([
    Schema.Struct({ distinctRegions: Int }),
    Schema.Struct({ oneRegion: Schema.Struct({ probes: Int, spacingMs: Int }) }),
  ]),
  observations: Schema.Array(IncidentObservation),
});
export type IncidentEvidence = typeof IncidentEvidence.Type;

const EnvironmentScope = {
  organizationSlug: Schema.NonEmptyString,
  environmentId: Schema.NonEmptyString,
};

export const ReadEnvironmentUptimeInput = Schema.Struct(EnvironmentScope);
export type ReadEnvironmentUptimeInput = typeof ReadEnvironmentUptimeInput.Type;

export const ReadServiceUptimeInput = Schema.Struct({
  ...EnvironmentScope,
  serviceId: Schema.NonEmptyString,
  from: RangeEdge,
  to: RangeEdge,
});
export type ReadServiceUptimeInput = typeof ReadServiceUptimeInput.Type;

/** One check of the environment, and when its open incident opened. */
export type EnvironmentUptimeCheck = {
  readonly serviceId: string;
  readonly url: string;
  readonly openIncidentAt: string | null;
};

/**
 * `unreachable` while the check has an open incident. `pending` when it has none and no probe result within raw
 * retention, so a new check never reads up before it was probed. `up` otherwise.
 */
export type UptimeStatus = "pending" | "up" | "unreachable";

/**
 * Why a failing probe failed. The first that matches wins, in this order, so every failing probe lands in exactly one:
 * an HTTP status, a timeout, a refused connection, the phase it broke in, anything else. `failureClassSql` mirrors it.
 */
export const FAILURE_CLASSES = ["status", "timeout", "refused", "dns", "connect", "tls", "other"] as const;
export type FailureClass = (typeof FAILURE_CLASSES)[number];
export type FailureCounts = Readonly<Record<FailureClass, number>>;

/** Each class in one word. A status failure reads as its code instead. */
export const FAILURE_WORDS = {
  timeout: "Timeout", refused: "Refused", dns: "DNS", connect: "Connect", tls: "TLS", other: "Error",
} as const satisfies Record<Exclude<FailureClass, "status">, string>;

export const PHASES = ["dns", "connect", "tls", "ttfb"] as const;
export type Phase = (typeof PHASES)[number];

/**
 * One region's probes over one stretch of the latency chart, as `pointSource` picks for the range: one routine probe
 * for a range up to a day within raw retention, an hour up to 7 days, 6 hours longer.
 * `probes` is pass plus fail, never inconclusive. `medianMs`, `maxMs` and `phaseMsSum` are over passing probes only,
 * and the first two are null when none passed. `lastError` is the newest failure. `mergePoints` folds two into one.
 */
export type UptimePoint = {
  readonly at: string;
  readonly region: string;
  readonly probes: number;
  readonly failures: FailureCounts;
  readonly medianMs: number | null;
  readonly maxMs: number | null;
  readonly phaseMsSum: Readonly<Record<Phase, number>>;
  readonly lastError: { readonly at: string; readonly cls: FailureClass; readonly text: string } | null;
};

/** One probe result of any kind, as a bar of the history strip. `status` is 0 when there was no HTTP response. */
export type UptimeCheckResult = {
  readonly at: string;
  readonly region: string;
  readonly kind: IncidentObservation["kind"];
  readonly verdict: IncidentObservation["verdict"];
  readonly status: number;
  readonly ms: number;
};

export type UptimeIncidentEntry = {
  readonly id: string;
  readonly openedAt: string;
  readonly closedAt: string | null;
  /** The probes that opened it, one line per region. */
  readonly observations: readonly IncidentObservation[];
};

/**
 * A check over one range. `uptime`, `checks` and `incidents` cover its check window, at least the last 30 minutes of
 * it; `points` cover the range itself, the time axis every chart shares.
 */
export type ServiceUptime = {
  readonly check: { readonly id: string; readonly url: string; readonly intervalSeconds: number };
  readonly status: UptimeStatus;
  /** Passing over pass plus fail routine probes, 0 to 1. Null when no probe counted. */
  readonly uptime: number | null;
  readonly points: readonly UptimePoint[];
  /**
   * Every result in a check window of 6 hours or less, the newest 120, oldest first. Null for a longer window, or one
   * older than raw retention, which the strip draws as spans of time.
   */
  readonly checks: readonly UptimeCheckResult[] | null;
  /** Open and closed incidents that overlap the check window, newest first. */
  readonly incidents: readonly UptimeIncidentEntry[];
};
