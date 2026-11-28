import type { MachineRuntime, RegisterRequest } from "@ployz/sdk";
import { Data, Effect, Schema, SchemaGetter } from "effect";
import type { JsonValue } from "#/db/tables";
import { rustMachineIdSchema, StrictOrganizationSlug, Uuid } from "#/lib/schema";
import { RecoveryBackup, SealingPublicKey } from "#/modules/sealing/sealing-key";

export class JoinApprovalRequired extends Data.TaggedError("JoinApprovalRequired")<{ readonly message: string }> {
  readonly publicErrorCategory = "conflict";
  readonly userFacing = true;
  readonly details = { capability: "ployz.machine.join.device-approval.v1" };
}

export const ENROLLMENT_TOKEN_TTL_MS = 24 * 60 * 60 * 1000;
export const ENROLL_NOT_YET_RETRY_AFTER_SECONDS = 2;
export const ENROLL_QUEUED_RETRY_AFTER_SECONDS = 5;
export const ENROLLMENT_PROTOCOL_VERSION = 1 as const;

/** The CLI names its Organization by ID, the dashboard by the slug in its URL. */
const OrganizationId = Uuid;

export const CliMintMachineEnrollmentInput = Schema.Struct({
  organizationId: OrganizationId,
  /** The CLI's own Device, which the line it prints names with `--key`. */
  firstKey: Schema.optionalKey(Schema.Struct({
    kind: Schema.Literal("device"),
    code: Schema.String.check(Schema.isPattern(/^[a-z2-7]{10}$/u)),
  })),
});

export type CliMintMachineEnrollmentInput =
  typeof CliMintMachineEnrollmentInput.Type;

export const ReadMachineEnrollmentInput = Schema.Struct({
  organizationId: OrganizationId,
  id: Schema.String.check(Schema.isUUID()),
});

export type ReadMachineEnrollmentInput =
  typeof ReadMachineEnrollmentInput.Type;

export const ResetPendingEnrollmentInput = Schema.Struct({
  organizationSlug: StrictOrganizationSlug,
  confirmedFounderStoppedOrErased: Schema.Literal(true),
});

export type ResetPendingEnrollmentInput =
  typeof ResetPendingEnrollmentInput.Type;

export const CliResetPendingEnrollmentInput = Schema.Struct({
  organizationId: OrganizationId,
  confirmedFounderStoppedOrErased: Schema.Literal(true),
});

export type CliResetPendingEnrollmentInput =
  typeof CliResetPendingEnrollmentInput.Type;

const WIREGUARD_PUBLIC_KEY_BYTES = 32;

const ENROLL_REGISTER_RUNTIME: MachineRuntime = {
  daemon_version: "",
  docker_version: "",
  hostname: "",
  architecture: "",
  os_pretty_name: "",
  kernel_version: "",
  running_builds: 0,
  capabilities: [],
};

function wireGuardPublicKeyFromDisplay(display: string): number[] | null {
  if (!/^[A-Za-z0-9+/]+={0,2}$/u.test(display) || display.length % 4 !== 0) {
    return null;
  }
  let binary: string;
  try {
    binary = atob(display);
  } catch {
    // Not base64, so not a key.
    return null;
  }
  if (binary.length !== WIREGUARD_PUBLIC_KEY_BYTES) return null;
  if (btoa(binary) !== display) return null;
  return Array.from(binary, (char) => char.charCodeAt(0));
}

const NonnegativeSafeInteger = Schema.Int.check(
  Schema.isGreaterThanOrEqualTo(0),
);

const MachineLabelKey = Schema.String.check(
  Schema.isTrimmed(),
  Schema.isPattern(/^[A-Za-z0-9_.-]+$/u),
);
const MachineLabelValue = Schema.String.check(
  Schema.isTrimmed(),
  Schema.isPattern(/^[A-Za-z0-9:_ .()*?+[\]\\^$|/-]+$/u),
);
const initialMachinePolicySchema = Schema.Struct({
  labels: Schema.Record(Schema.String, MachineLabelValue).check(
    Schema.makeFilter(
      (labels) => Object.keys(labels).every(Schema.is(MachineLabelKey)),
      { message: "Machine Label keys must contain only ASCII letters, digits, '_', '.', or '-'." },
    ),
  ),
  accepts_builds: Schema.Boolean,
  accepts_services: Schema.Boolean,
  accepts_ingress: Schema.Boolean,
});

export const enrollmentIdentitySchema = Schema.Struct({
  protocolVersion: Schema.Literal(ENROLLMENT_PROTOCOL_VERSION),
  name: Schema.NonEmptyString,
  initialPolicy: initialMachinePolicySchema,
  machineId: rustMachineIdSchema,
  publicKey: Schema.NonEmptyString.check(
    Schema.makeFilter((value) => wireGuardPublicKeyFromDisplay(value) !== null, {
      message: "publicKey must be a WireGuard Display base64 key.",
    }),
  ),
  advertisedEndpoints: Schema.Array(Schema.NonEmptyString),
  publicIp: Schema.optionalKey(Schema.NullOr(Schema.NonEmptyString)),
  requestedStorage: Schema.Literals(["none", "zfs"]).pipe(
    Schema.withDecodingDefaultKey(Effect.succeed("none")),
  ),
  memoryTotalBytes: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
  diskTotalBytes: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
  diskAvailableBytes: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
  recoverySupport: Schema.optionalKey(Schema.Literal(1)),
  /** The joiner's proof that it opened its approved capsule. Cloud relays it in Register; it opens nothing. */
  joinConfirmation: Schema.optionalKey(Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/u))),
  /** What the client that runs this joiner's catch-up honors, as capability names. Absent claims nothing. Cloud relays it in Register. */
  capabilities: Schema.optionalKey(Schema.Array(Schema.String)),
  /**
   * The key the joiner's install command named, which it says it waits for. Cloud relays it in Register so whoever
   * approves the join signs with that key. A hint: the joiner checks its approval against its own file.
   */
  firstKey: Schema.optionalKey(Schema.Struct({
    kind: Schema.Literals(["device", "passkey"]),
    code: Schema.String.check(Schema.isPattern(/^[a-z2-7]{10}$/u)),
  })),
});

export type EnrollmentIdentity = typeof enrollmentIdentitySchema.Type;

/**
 * HTTP enroll identity → SDK RegisterRequest. CLI does not POST runtime. The joiner's `capabilities`, `joinConfirmation`
 * and `firstKey` are left out: they ride only the Register Cloud dispatches, never the saved assignment a retry compares.
 */
export function registerRequestFromEnrollmentIdentity(
  identity: EnrollmentIdentity,
): RegisterRequest {
  const publicKey = wireGuardPublicKeyFromDisplay(identity.publicKey);
  if (publicKey === null) {
    throw new RangeError("publicKey must be a WireGuard Display base64 key.");
  }
  return {
    name: identity.name,
    initial_policy: identity.initialPolicy,
    storage: identity.requestedStorage,
    machine_id: identity.machineId,
    assigned_subnet: null,
    public_key: publicKey,
    public_ip: identity.publicIp ?? null,
    advertised_endpoints: [...identity.advertisedEndpoints],
    runtime: ENROLL_REGISTER_RUNTIME,
  };

}

/** `ployz1:` plus unpadded base64url of a 64-byte body. */
export const MANAGEMENT_CAPABILITY_LENGTH = "ployz1:".length + 86;

const enrollmentCallbackIdentity = {
  machineId: rustMachineIdSchema,
  pairingCredential: Schema.NonEmptyString,
};

/** Whether the enrolled Machine holds its Cluster's Sealing Key, and its public half if so. */
const Sealing = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("held"), publicKey: SealingPublicKey }),
  Schema.Struct({ kind: Schema.Literal("needs_access") }),
]);

const FounderRecoveryBackup = Schema.Struct({ publicKey: SealingPublicKey, backup: RecoveryBackup });

export const enrollmentCallbackBodySchema = Schema.Union([
  Schema.Struct({
    ...enrollmentCallbackIdentity,
    sealing: Schema.optionalKey(Sealing),
    recoveryBackup: Schema.optionalKey(FounderRecoveryBackup),
  }),
  Schema.Struct({
    ...enrollmentCallbackIdentity,
    stage: Schema.Literal("publish"),
    capability: Schema.NonEmptyString.check(Schema.isMaxLength(MANAGEMENT_CAPABILITY_LENGTH)),
  }),
  /** A founder that refused to found lets go of its Founding Claim, so the Organization can start with another Server. */
  Schema.Struct({
    ...enrollmentCallbackIdentity,
    stage: Schema.Literal("release"),
  }),
]);

export type EnrollmentCallback = typeof enrollmentCallbackBodySchema.Type;
export type EnrollmentCallbackSealing = typeof Sealing.Type;

/** The phases of token enrollment; each name is a PostHog property, so renaming one breaks analytics. */
const SetupStep = Schema.Literals(["install", "enroll", "storage", "join"]);
const Seconds = Schema.Finite.check(Schema.isGreaterThanOrEqualTo(0));
/** `schema`, with `clean` applied on decode; encoding leaves the text as is.
 * Long text is cut rather than rejected, so a long field never loses the report. */
const cleaned = (schema: Schema.String, clean: (value: string) => string) =>
  schema.pipe(Schema.decodeTo(Schema.String, {
    decode: SchemaGetter.transform(clean),
    encode: SchemaGetter.transform((value) => value),
  }));
const ProfileText = cleaned(Schema.NonEmptyString, (value) => value.slice(0, 256));

// Not part of a longer dotted number, but a sentence's closing period still ends it.
const IPV4 = /(?<!\d\.?)(?:\d{1,3}\.){3}\d{1,3}(?!\.?\d)/gu;
// Eight full groups, or any groups around `::`, with an optional zone. Needing `::` or eight groups
// spares times (12:34:56), MAC addresses and Rust paths (`std::io` is not hex).
const IPV6 = /(?<!\w)(?:(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}|(?:[0-9a-f]{1,4}:)*[0-9a-f]{0,4}::(?:[0-9a-f]{1,4}:)*[0-9a-f]{0,4})(?:%[\w.-]+)?(?![\w:])/giu;

/** An error the user saw, with every IP address replaced by `<ip>`; hostnames stay. */
export function redactIpAddresses(text: string) {
  return text.replace(IPV4, "<ip>").replace(IPV6, "<ip>");
}

/** Fields both outcomes carry; unknown fields from newer CLIs are dropped. */
const setupReportFields = {
  profile: Schema.Struct({
    provider: Schema.optionalKey(ProfileText),
    instanceType: Schema.optionalKey(ProfileText),
    osId: Schema.optionalKey(ProfileText),
    osVersion: Schema.optionalKey(ProfileText),
    kernel: Schema.optionalKey(ProfileText),
    arch: Schema.optionalKey(ProfileText),
    virtualization: Schema.optionalKey(ProfileText),
    cpuCount: Schema.optionalKey(NonnegativeSafeInteger),
    memoryTotalBytes: Schema.optionalKey(NonnegativeSafeInteger),
    diskTotalBytes: Schema.optionalKey(NonnegativeSafeInteger),
    storage: Schema.optionalKey(ProfileText),
    ployzVersion: Schema.optionalKey(ProfileText),
    founder: Schema.optionalKey(Schema.Boolean),
  }),
  steps: Schema.Array(Schema.Struct({ name: SetupStep, seconds: Seconds })),
  totalSeconds: Seconds,
};

/** What token enrollment reports about its Server and how setup went. */
export const setupReportSchema = Schema.Union([
  Schema.Struct({ outcome: Schema.Literal("succeeded"), ...setupReportFields }),
  Schema.Struct({
    outcome: Schema.Literal("failed"),
    ...setupReportFields,
    failedStep: SetupStep,
    failedStepSeconds: Seconds,
    error: cleaned(Schema.String, (value) => redactIpAddresses(value).slice(0, 1_000)),
  }),
]);

export type SetupReport = typeof setupReportSchema.Type;

/** The Cloud Enroll Token a setup report is for: who made it, and whether it joined or expired. */
export type EnrollmentTokenRow = {
  readonly userId: string;
  readonly organizationId: string;
  readonly joinedMachineId: string | null;
  readonly expiresAt: Date;
};

/**
 * Success counts only once the token joined a Machine; failure only while it is still pending,
 * so a token can't be used to inject events after the fact.
 */
export function setupReportAccepted(
  row: EnrollmentTokenRow | undefined,
  outcome: SetupReport["outcome"],
  now: Date,
): row is EnrollmentTokenRow {
  if (row === undefined) return false;
  if (outcome === "succeeded") return row.joinedMachineId !== null;
  return row.joinedMachineId === null && row.expiresAt.getTime() > now.getTime();
}

const snakeCase = (key: string) => key.replace(/[A-Z]/gu, (letter) => `_${letter.toLowerCase()}`);

/** The report as flat snake_case PostHog properties, so each one charts and breaks down directly. */
export function setupReportProperties(report: SetupReport) {
  const properties: Record<string, string | number | boolean> = {};
  for (const [key, value] of Object.entries(report.profile)) {
    properties[snakeCase(key)] = value;
  }
  for (const step of report.steps) properties[`step_${step.name}_seconds`] = step.seconds;
  properties["total_seconds"] = report.totalSeconds;
  if (report.outcome === "failed") {
    properties["failed_step"] = report.failedStep;
    properties["failed_step_seconds"] = report.failedStepSeconds;
    properties["error"] = report.error;
  }
  return properties;
}

export type CloudPairing = {
  secret: string;
};

/** A founding Server backs up the Sealing Key it makes, or waits for its Organization's key to be recovered. */
export type FoundingRecovery =
  | { kind: "back_up" }
  | { kind: "seed"; expectedPublicKey: string; backup: string | null };

export type InitializeJoinMaterial = {
  kind: "initialize";
  resumed: boolean;
  pairing: CloudPairing;
  storage: RegisterRequest["storage"];
  recovery?: FoundingRecovery | undefined;
};

export type JoinEnrollMaterial = {
  kind: "join";
  pairing: CloudPairing;
  storage: RegisterRequest["storage"];
  registration: JsonValue;
};

/** `reason` and `ahead` are optional on the wire: a CLI that predates them retries as for any `not_yet`. */
export type NotYetEnrollMaterial = {
  kind: "not_yet";
  retryAfter: number;
  reason?: "founder" | "queued";
  /**
   * An estimate of the joins that must finish before this one may start. It can rise again when a
   * waiter that went quiet returns to its old place.
   */
  ahead?: number;
};

export type EnrollResponse =
  | InitializeJoinMaterial
  | JoinEnrollMaterial
  | NotYetEnrollMaterial;

export type OrganizationEnrollmentStatus = "unclaimed" | "pending" | "ready";

/** The organization's pairing row (at most one) as seen by Cloud. */
export type OrganizationEnrollmentRow = { id: string; status: Exclude<OrganizationEnrollmentStatus, "unclaimed"> };

/** A pairing is pending until its founder machine completes the claim. */
export function pairingEnrollmentStatus(founderMachineId: string | null): OrganizationEnrollmentRow["status"] {
  return founderMachineId === null ? "pending" : "ready";
}

export function organizationEnrollmentStatus(row: OrganizationEnrollmentRow | undefined): OrganizationEnrollmentStatus {
  return row?.status ?? "unclaimed";
}

export function waitForFounder(): NotYetEnrollMaterial {
  return { kind: "not_yet", retryAfter: ENROLL_NOT_YET_RETRY_AFTER_SECONDS, reason: "founder" };
}

export function waitForJoinSlot(ahead: number): NotYetEnrollMaterial {
  return { kind: "not_yet", retryAfter: ENROLL_QUEUED_RETRY_AFTER_SECONDS, reason: "queued", ahead };
}
