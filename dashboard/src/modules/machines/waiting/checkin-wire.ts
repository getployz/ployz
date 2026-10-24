import { Schema } from "effect";
import type { EnrollInstruction, EnrollmentRole } from "#/modules/machines/waiting/server-enrollment";

export const CHECKIN_PROTOCOL_VERSION = 1 as const;
export const CHECKIN_MAX_BYTES = 64 * 1024;
export const CHECKIN_WAIT_SECONDS = 10;

const Issue = Schema.Struct({ code: Schema.String, message: Schema.String });
const Count = Schema.Finite.check(Schema.isInt(), Schema.isGreaterThanOrEqualTo(0));

export const HostReport = Schema.Struct({
  hostname: Schema.String,
  publicIp: Schema.optionalKey(Schema.String),
  provider: Schema.optionalKey(Schema.String),
  os: Schema.Struct({ id: Schema.String, version: Schema.String }),
  kernel: Schema.String,
  arch: Schema.String,
  virt: Schema.String,
  cpuCount: Count,
  memoryTotalBytes: Count,
  disks: Schema.Array(Schema.Struct({
    path: Schema.String,
    byId: Schema.optionalKey(Schema.String),
    sizeBytes: Count,
    role: Schema.Literals(["root", "empty"]),
    availableBytes: Schema.optionalKey(Count),
  })),
  docker: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("absent") }),
    Schema.Struct({ kind: Schema.Literal("present"), version: Schema.String, api: Schema.String, compatible: Schema.Boolean }),
  ]),
  cgroup: Schema.Literals(["v1", "v2", "hybrid"]),
  zfs: Schema.Union([
    Schema.Struct({
      kind: Schema.Literal("eligible"),
      targets: Schema.Array(Schema.Union([
        Schema.Struct({ kind: Schema.Literal("main_disk") }),
        Schema.Struct({ kind: Schema.Literal("disk"), byId: Schema.String }),
      ])),
      buildMinutes: Schema.optionalKey(Schema.Finite),
    }),
    Schema.Struct({ kind: Schema.Literal("ineligible"), code: Schema.String, message: Schema.String }),
  ]),
  blockers: Schema.Array(Issue),
  warnings: Schema.Array(Issue),
});
export type HostReport = typeof HostReport.Type;

export const Prep = Schema.Struct({
  docker: Schema.Literals(["installing", "ready", "failed"]),
  zfs: Schema.Literals(["installing", "ready", "failed", "skipped", "off"]),
});
export type Prep = typeof Prep.Type;

export const CheckinKey = Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/));

export const CheckinRequest = Schema.Struct({
  protocolVersion: Schema.Literal(CHECKIN_PROTOCOL_VERSION),
  checkinKey: CheckinKey,
  cliVersion: Schema.String,
  report: HostReport,
  attempt: Schema.optionalKey(Schema.Struct({
    attemptId: Schema.String,
    stage: Schema.String,
    failure: Schema.optionalKey(Schema.Struct({ stage: Schema.String, message: Schema.String })),
  })),
  probe: Schema.optionalKey(Schema.Struct({
    handout: Schema.String,
    heardFrom: Schema.Array(Schema.String),
    bindFailed: Schema.Array(Schema.String),
  })),
  prep: Schema.optionalKey(Prep),
  /** The Local Machine Phase the host's ployzd reported; absent from older CLIs and when ployzd didn't answer. */
  machinePhase: Schema.optionalKey(Schema.String),
});
export type CheckinRequest = typeof CheckinRequest.Type;

export type StoredReport = HostReport & { readonly prep?: Prep; readonly cliVersion?: string; readonly machinePhase?: string };

export type CheckinStopReason = "rejected" | "token_expired" | "token_revoked";

export const PROBE_WINDOW_MS = 10_000;

export const ProbeRequest = Schema.Struct({
  nonce: Schema.String,
  windowMs: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: PROBE_WINDOW_MS })),
  listen: Schema.Array(Schema.Literals(["udp:51820", "tcp:80", "tcp:443"])),
  send: Schema.Array(Schema.Struct({ target: Schema.String, port: Schema.Literal(51820) })),
});
export type ProbeRequest = typeof ProbeRequest.Type;

export type ProbeHandout = ProbeRequest & { readonly handout: string };

export type CheckinResponse =
  | { readonly kind: "wait"; readonly retryAfter: number; readonly probe?: ProbeHandout; readonly noCluster?: true }
  | { readonly kind: "stop"; readonly reason: CheckinStopReason; readonly message: string }
  | { readonly kind: "enroll"; readonly attemptId: string; readonly token: string; readonly role: EnrollmentRole } & EnrollInstruction;
