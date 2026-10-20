import { Schema } from "effect";
import type { StoredReport } from "#/modules/machines/waiting/checkin-wire";

export type EnrollmentRole = "founder" | "joiner";

export type EnrollmentState = "awaiting_founder" | "instructed" | "running" | "joined" | "failed" | "cancelled";

export type EnrollmentStage = "installing" | "storage" | "enrolling" | "queued" | "joining" | "publishing";

export const ENROLLMENT_STAGES: ReadonlyArray<EnrollmentStage> = ["installing", "enrolling", "queued", "storage", "joining", "publishing"];

/** The stages a host reports once its daemon has committed the join. */
export const COMMITTED_STAGES = ENROLLMENT_STAGES.slice(ENROLLMENT_STAGES.indexOf("storage") + 1);

export const PAST_STORAGE: ReadonlySet<string | null> = new Set(COMMITTED_STAGES);

/** The committed stages further along than `stage`. A report of `stage` arriving late must not replace them. */
export const committedAfter = (stage: string | null): ReadonlyArray<EnrollmentStage> =>
  COMMITTED_STAGES.slice(COMMITTED_STAGES.findIndex((known) => known === stage) + 1);

/** Whether the host's daemon has committed this attempt's join, by the run's record or by the host's report on its waiting row. */
export const joinCommitted = (
  attempt: { readonly id: string; readonly stage: string | null },
  row: { readonly reportedAttemptId: string | null; readonly reportedStage: string | null },
) => PAST_STORAGE.has(attempt.stage) || (row.reportedAttemptId === attempt.id && PAST_STORAGE.has(row.reportedStage));

export const OPEN_ENROLLMENT_STATES: ReadonlyArray<EnrollmentState> = ["awaiting_founder", "instructed", "running"];

export type EnrollStorage = { readonly kind: "none" } | { readonly kind: "zfs"; readonly device?: string };

export type EnrollPolicy = {
  readonly labels: Readonly<Record<string, string>>;
  readonly acceptsBuilds: boolean;
  readonly acceptsServices: boolean;
  readonly acceptsIngress: boolean;
};

/** What the host runs once Cloud answers `enroll`; the token travels beside it, never inside it. */
export type EnrollInstruction = {
  readonly name: string;
  readonly storage: EnrollStorage;
  readonly policy: EnrollPolicy;
};

export type EnrollmentFailure = { readonly stage: EnrollmentStage | null; readonly message: string };

/** The first beta that enrolls from Cloud plans. */
export const ENROLL_MIN_CLI_VERSION = "0.3.0-beta.6";

export const ZFS_BUILD_FAILED = "ZFS build failed";

export const EnrollInput = Schema.Struct({
  ids: Schema.Array(Schema.String).check(Schema.isMinLength(1)),
  first: Schema.optionalKey(Schema.String),
  storage: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("zfs"), useSpareDisk: Schema.Boolean, fallbackToDocker: Schema.Boolean }),
    Schema.Struct({ kind: Schema.Literal("none") }),
  ]),
  names: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("hostnames") }),
    Schema.Struct({ kind: Schema.Literal("pattern"), pattern: Schema.String }),
  ]),
  runs: Schema.Struct({ services: Schema.Boolean, ingress: Schema.Boolean, builds: Schema.Boolean }),
});
export type EnrollInput = typeof EnrollInput.Type;

export function isReadyFor(report: StoredReport, storage: EnrollStorage) {
  if (report.blockers.length > 0) return false;
  const { prep } = report;
  if (prep === undefined) return true;
  if (prep.docker !== "ready") return false;
  return storage.kind !== "zfs" || prep.zfs === "ready";
}
