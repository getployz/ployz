import { Schema } from "effect";

export const UPTIME_RANGES = ["24h", "7d", "90d"] as const;
export type UptimeRange = (typeof UPTIME_RANGES)[number];

export const UPTIME_RANGE_MS = {
  "24h": 24 * 3_600_000,
  "7d": 7 * 86_400_000,
  "90d": 90 * 86_400_000,
} as const satisfies Record<UptimeRange, number>;

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

export const ReadServiceUptimeInput = Schema.Struct({ ...EnvironmentScope, serviceId: Schema.NonEmptyString });
export type ReadServiceUptimeInput = typeof ReadServiceUptimeInput.Type;

export const ReadServiceUptimeSeriesInput = Schema.Struct({
  ...EnvironmentScope,
  serviceId: Schema.NonEmptyString,
  range: Schema.Literals(UPTIME_RANGES),
});
export type ReadServiceUptimeSeriesInput = typeof ReadServiceUptimeSeriesInput.Type;

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
 * One point of the latency chart. In 24h it is one routine probe: `ms` is its total time when it passed and null when
 * it failed or was inconclusive. In 7d it is one region's hour: `ms` is the median total time of its passing probes.
 * In 90d it is one region's 6 hours: `ms` is the hourly medians averaged by passing probes. Null when none passed.
 */
export type UptimePoint = { readonly at: string; readonly region: string; readonly ms: number | null };

export type UptimeIncidentEntry = {
  readonly id: string;
  readonly openedAt: string;
  readonly closedAt: string | null;
  /** The probes that opened it, one line per region. */
  readonly observations: readonly IncidentObservation[];
};

export type ServiceUptime = {
  readonly check: { readonly id: string; readonly url: string; readonly intervalSeconds: number };
  readonly status: UptimeStatus;
  /** Passing over pass plus fail routine probes, 0 to 1, by rolled-up hour. Null when no probe counted. */
  readonly uptime: Readonly<Record<UptimeRange, number | null>>;
  /** The last 90 UTC days, oldest first. `checked` when any hour of the day was rolled up. */
  readonly days: readonly { readonly start: string; readonly checked: boolean }[];
  /** Open and closed incidents that overlap the last 90 days, newest first. */
  readonly incidents: readonly UptimeIncidentEntry[];
};
