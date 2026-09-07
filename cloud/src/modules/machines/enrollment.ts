import type { MachineId, MachineRuntime, RegisterRequest } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import type { JsonValue } from "#/db/tables";
import { type DialTenant } from "#/modules/runtime/dial-entry";
import type { PloyzProviderError } from "#/modules/runtime/ployz.server";

export const MACHINE_ID_PATTERN = /^[0-9a-f]{32}$/u;
export const ENROLLMENT_TOKEN_TTL_MS = 24 * 60 * 60 * 1000;
export const ENROLL_NOT_YET_RETRY_AFTER_SECONDS = 2;
export const ENROLLMENT_PROTOCOL_VERSION = 2 as const;

const OrganizationSlug = Schema.String.check(
  Schema.isTrimmed(),
  Schema.isNonEmpty(),
);

export const MintMachineEnrollmentInput = Schema.Struct({
  organizationSlug: OrganizationSlug,
});

export type MintMachineEnrollmentInput =
  typeof MintMachineEnrollmentInput.Type;

export const ResetPendingEnrollmentInput = Schema.Struct({
  organizationSlug: OrganizationSlug,
  confirmedFounderStoppedOrErased: Schema.Literal(true),
});

export type ResetPendingEnrollmentInput =
  typeof ResetPendingEnrollmentInput.Type;

const MachineIdType = Schema.declare<MachineId>(
  (value): value is MachineId => typeof value === "string",
);

export const rustMachineIdSchema = Schema.String.check(
  Schema.isPattern(MACHINE_ID_PATTERN, {
    message: "MachineId must be a 32-hex UUID",
  }),
).pipe(Schema.decodeTo(MachineIdType));

/** One Relay List row. The Relay speaks JSON; this is the parse at the seam. */
export const heldRegisterSchema = Schema.Struct({
  machineId: rustMachineIdSchema,
});

const WIREGUARD_PUBLIC_KEY_BYTES = 32;

const ENROLL_REGISTER_RUNTIME: MachineRuntime = {
  daemon_version: "",
  docker_version: "",
  hostname: "",
  architecture: "",
  os_pretty_name: "",
  kernel_version: "",
};

function wireGuardPublicKeyFromDisplay(display: string): number[] | null {
  if (!/^[A-Za-z0-9+/]+={0,2}$/u.test(display) || display.length % 4 !== 0) {
    return null;
  }
  let binary: string;
  try {
    binary = atob(display);
  } catch {
    return null;
  }
  if (binary.length !== WIREGUARD_PUBLIC_KEY_BYTES) return null;
  if (btoa(binary) !== display) return null;
  return Array.from(binary, (char) => char.charCodeAt(0));
}

/** Version 2 is a coordinated break: older CLIs must upgrade before founding. */
const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const NonnegativeSafeInteger = Schema.Int.check(
  Schema.isGreaterThanOrEqualTo(0),
);

export const enrollmentIdentitySchema = Schema.Struct({
  protocolVersion: Schema.Literal(ENROLLMENT_PROTOCOL_VERSION),
  name: NonEmptyString,
  publicKey: NonEmptyString.check(
    Schema.makeFilter((value) => wireGuardPublicKeyFromDisplay(value) !== null, {
      message: "publicKey must be a WireGuard Display base64 key.",
    }),
  ),
  advertisedEndpoints: Schema.Array(NonEmptyString),
  publicIp: Schema.optionalKey(Schema.NullOr(NonEmptyString)),
  requestedStorage: Schema.Literals(["none", "zfs"]).pipe(
    Schema.withDecodingDefaultKey(Effect.succeed("none")),
  ),
  memoryTotalBytes: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
  diskTotalBytes: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
  diskAvailableBytes: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
});

export type EnrollmentIdentity = typeof enrollmentIdentitySchema.Type;

/** HTTP enroll identity → SDK RegisterRequest. CLI does not POST runtime. */
export function registerRequestFromEnrollmentIdentity(
  identity: EnrollmentIdentity,
): RegisterRequest {
  const publicKey = wireGuardPublicKeyFromDisplay(identity.publicKey);
  if (publicKey === null) {
    throw new RangeError("publicKey must be a WireGuard Display base64 key.");
  }
  return {
    name: identity.name,
    storage: identity.requestedStorage,
    public_key: publicKey,
    public_ip: identity.publicIp ?? null,
    advertised_endpoints: [...identity.advertisedEndpoints],
    runtime: ENROLL_REGISTER_RUNTIME,
  };

}

export const enrollmentCallbackBodySchema = Schema.Struct({
  machineId: rustMachineIdSchema,
  pairingCredential: NonEmptyString,
});

export type CloudPairing = {
  relayUrl: string;
  secret: string;
};

export type OrganizationDialAccess =
  | { kind: "missing" }
  | {
      kind: "unreachable";
      cause: "empty" | "indeterminate";
      error: PloyzProviderError | null;
    }
  | { kind: "ready"; tenant: DialTenant };

export type InitializeJoinMaterial = {
  kind: "initialize";
  resumed: boolean;
  pairing: CloudPairing;
  storage: RegisterRequest["storage"];
};

export type JoinEnrollMaterial = {
  kind: "join";
  pairing: CloudPairing;
  storage: RegisterRequest["storage"];
  registration: JsonValue;
};

export type NotYetEnrollMaterial = {
  kind: "not_yet";
  retryAfter: number;
};

export type EnrollResponse =
  | InitializeJoinMaterial
  | JoinEnrollMaterial
  | NotYetEnrollMaterial;

export type MintedMachineEnrollment = {
  command: string;
  expiresAt: string;
};

export type OrganizationEnrollmentStatus = "unclaimed" | "pending" | "ready";

/**
 * What Cloud sees on the Relay List for an organization.
 *
 * `held` carries parsed Machine ids and is never empty: a List Cloud cannot
 * read an id out of is `indeterminate`, as is a Relay it cannot reach. Both
 * are kept distinct from `empty` so neither reads as "nobody holds a
 * Register" — which would revoke a pairing a live founder may still hold.
 */
export type EnrollListObservation =
  | { kind: "missing" }
  | { kind: "empty"; pairing: CloudPairing; bearer: string }
  | {
      kind: "held";
      pairing: CloudPairing;
      bearer: string;
      held: readonly MachineId[];
    }
  | { kind: "indeterminate"; error: PloyzProviderError };

export type HeldRelayList = Extract<EnrollListObservation, { kind: "held" }>;

export function enrollmentExpiry(now: Date) {
  return new Date(now.getTime() + ENROLLMENT_TOKEN_TTL_MS);
}

const DEFAULT_CLOUD_URL_HOST = "ployz.dev";

export function buildMachineJoinCommand(input: {
  installerUrl: string;
  token: string;
  origin: string;
}) {
  const host = new URL(input.origin).hostname;
  const cloudUrlFlag =
    host === DEFAULT_CLOUD_URL_HOST ? "" : ` --cloud-url '${input.origin}'`;
  return `curl -fsSL ${input.installerUrl} | sh && sudo ployz cloud enroll '${input.token}'${cloudUrlFlag}`;
}

export function mintedEnrollment(input: {
  installerUrl: string;
  origin: string;
  token: string;
  expiresAt: Date;
}): MintedMachineEnrollment {
  return {
    command: buildMachineJoinCommand({
      installerUrl: input.installerUrl,
      token: input.token,
      origin: input.origin,
    }),
    expiresAt: input.expiresAt.toISOString(),
  };
}

export function waitForFounder(): NotYetEnrollMaterial {
  return {
    kind: "not_yet",
    retryAfter: ENROLL_NOT_YET_RETRY_AFTER_SECONDS,
  };
}

export function enrollmentDialTenant(input: {
  relayUrl: string;
  bearer: string;
  pairing: string;
  held: readonly string[];
}): DialTenant {
  return {
    relayUrl: input.relayUrl,
    bearer: input.bearer,
    pairing: input.pairing,
    preferredMachineId: input.held[0] ?? null,
    enrolledMachineIds: [...input.held],
  };
}

/**
 * Pairing is Cloud's claim that a Cluster exists. Membership is never the
 * Relay List: List hops are how Cloud Dials, and an empty or unreadable List
 * means the Cluster is expected but unreachable — not authoritative zero.
 */
export function dialAccessFromRelayList(input: {
  list: EnrollListObservation;
  dialUrl: string;
}): OrganizationDialAccess {
  switch (input.list.kind) {
    case "missing":
      return { kind: "missing" };
    case "empty":
      return { kind: "unreachable", cause: "empty", error: null };
    case "indeterminate":
      return {
        kind: "unreachable",
        cause: "indeterminate",
        error: input.list.error,
      };
    case "held":
      return {
        kind: "ready",
        tenant: enrollmentDialTenant({
          relayUrl: input.dialUrl,
          bearer: input.list.bearer,
          pairing: input.list.pairing.secret,
          held: input.list.held,
        }),
      };
    default: {
      const exhaustive: never = input.list;
      return exhaustive;
    }
  }
}
