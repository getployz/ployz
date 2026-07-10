import {
  MAX_LOGS_TAIL_LINES,
  MAX_OPERATION_EVENT_REPLAY_LIMIT,
  type AcmeChallengeToken,
  type AcmeChallengeTtlSeconds,
  type AcmeChallengeValue,
  type CancellationReason,
  type CertBundleRef,
  type CertId,
  type CertValidAt,
  type ContainerId,
  type ContainerMountPath,
  type DeployReservationId,
  type EventSequence,
  type FailureMessage,
  type ImageReference,
  type InstallArtifactVersion,
  type LogsTailLines,
  type MachineBootstrapUrl,
  type MachineJoinToken,
  type MachineName,
  type MachineId,
  type NamespaceId,
  type OperationEventReplayLimit,
  type OperationId,
  type OperationIdempotencyKey,
  type OperatorHint,
  type ReplicaCount,
  type NamespaceRevisionEntryId,
  type NamespaceRevisionId,
  type RouteHostname,
  type RoutePort,
  type ServiceId,
  type StopGracePeriod,
  type VolumeName,
} from "./generated.ts";

type U64WireInput = number | string | bigint;

export function operationId(value: string): OperationId {
  return subjectToken(value, "operation id") as OperationId;
}

export function namespaceId(value: string): NamespaceId {
  return subjectToken(value, "namespace id") as NamespaceId;
}

export function operationIdempotencyKey(value: string): OperationIdempotencyKey {
  return subjectToken(value, "operation idempotency key") as OperationIdempotencyKey;
}

export function serviceId(value: string): ServiceId {
  return subjectToken(value, "service id") as ServiceId;
}

export function namespaceRevisionId(value: string): NamespaceRevisionId {
  return subjectToken(value, "namespace revision id") as NamespaceRevisionId;
}

export function namespaceRevisionEntryId(value: string): NamespaceRevisionEntryId {
  return subjectToken(value, "namespace revision entry id") as NamespaceRevisionEntryId;
}

export function machineId(value: string): MachineId {
  return subjectToken(value, "machine id") as MachineId;
}

export function machineName(value: string): MachineName {
  return subjectToken(value, "machine name") as MachineName;
}

export function machineBootstrapUrl(value: string): MachineBootstrapUrl {
  if (value.trim() === "") {
    throw new RangeError("machine bootstrap URL must not be empty");
  }
  if (!value.startsWith("https://") || /[\s\p{C}]/u.test(value)) {
    throw new RangeError("machine bootstrap URL must be HTTPS and contain no invisible characters");
  }

  return value as MachineBootstrapUrl;
}

export function machineJoinToken(value: string): MachineJoinToken {
  if (value === "") {
    throw new RangeError("machine join token must not be empty");
  }
  if (/[\s\p{C}]/u.test(value)) {
    throw new RangeError("machine join token must contain no invisible characters");
  }

  return value as MachineJoinToken;
}

export function containerId(value: string): ContainerId {
  return subjectToken(value, "container id") as ContainerId;
}

export function certId(value: string): CertId {
  return subjectToken(value, "cert id") as CertId;
}

export function imageReference(value: string): ImageReference {
  if (value.trim() === "") {
    throw new RangeError("image reference must not be empty");
  }
  if (/[\s\p{C}]/u.test(value)) {
    throw new RangeError("image reference must not contain whitespace or control characters");
  }

  return value as ImageReference;
}

export function volumeName(value: string): VolumeName {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new RangeError("volume name must contain only ASCII letters, digits, '_' or '-'");
  }

  return value as VolumeName;
}

export function containerMountPath(value: string): ContainerMountPath {
  if (!value.startsWith("/") || value.includes("\0")) {
    throw new RangeError("container mount path must be an absolute path without NUL");
  }

  return value as ContainerMountPath;
}

export function installArtifactVersion(value: string): InstallArtifactVersion {
  return visibleAscii(value, "install artifact version") as InstallArtifactVersion;
}

export function failureMessage(value: string): FailureMessage {
  return nonEmptyText(value, "failure message") as FailureMessage;
}

export function cancellationReason(value: string): CancellationReason {
  return nonEmptyText(value, "cancellation reason") as CancellationReason;
}

export function operatorHint(value: string): OperatorHint {
  return nonEmptyText(value, "operator hint") as OperatorHint;
}

export function replicaCount(value: number): ReplicaCount {
  return positiveU16(value, "replica count") as ReplicaCount;
}

export function eventSequence(value: U64WireInput): EventSequence {
  return positiveU64String(value, "event sequence") as EventSequence;
}

export function deployReservationId(value: U64WireInput): DeployReservationId {
  return positiveU64String(value, "deploy reservation id") as DeployReservationId;
}

export function certValidAt(value: U64WireInput): CertValidAt {
  return positiveU64String(value, "certificate validity timestamp") as CertValidAt;
}

export function acmeChallengeTtlSeconds(value: U64WireInput): AcmeChallengeTtlSeconds {
  return positiveU64String(value, "ACME challenge TTL") as AcmeChallengeTtlSeconds;
}

export function operationEventReplayLimit(
  value: number | OperationEventReplayLimit,
): OperationEventReplayLimit {
  if (!Number.isSafeInteger(value) || value < 1 || value > MAX_OPERATION_EVENT_REPLAY_LIMIT) {
    throw new RangeError(
      `operation event replay limit must be an integer from 1 to ${MAX_OPERATION_EVENT_REPLAY_LIMIT}`,
    );
  }

  return value as OperationEventReplayLimit;
}

export function logsTailLines(value: number | LogsTailLines): LogsTailLines {
  if (!Number.isSafeInteger(value) || value < 1 || value > MAX_LOGS_TAIL_LINES) {
    throw new RangeError(`logs tail lines must be an integer from 1 to ${MAX_LOGS_TAIL_LINES}`);
  }

  return value as LogsTailLines;
}

export function routeHostname(value: string): RouteHostname {
  if (
    value === "" ||
    value.split(".").some((label) => label === "" || label.startsWith("-") || label.endsWith("-")) ||
    !/^[A-Za-z0-9.-]+$/.test(value)
  ) {
    throw new RangeError("route hostname is invalid");
  }

  return value.toLowerCase() as RouteHostname;
}

export function routePort(value: number): RoutePort {
  return positiveU16(value, "route port") as RoutePort;
}

export function stopGracePeriod(value: number): StopGracePeriod {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new RangeError("stop grace period must be an integer number of seconds within u32 range");
  }

  return value as StopGracePeriod;
}

export function acmeChallengeToken(value: string): AcmeChallengeToken {
  if (value === "") {
    throw new RangeError("ACME challenge token must not be empty");
  }
  if (!/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new RangeError("ACME challenge token is invalid");
  }

  return value as AcmeChallengeToken;
}

export function acmeChallengeValue(value: string): AcmeChallengeValue {
  const visible = visibleAscii(value, "ACME challenge value");
  if (!/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(visible)) {
    throw new RangeError("ACME challenge value is invalid");
  }

  return visible as AcmeChallengeValue;
}

export function certBundleRef(value: string): CertBundleRef {
  const visible = visibleAscii(value, "cert bundle reference");
  const match = visible.match(/^sha256:([A-Fa-f0-9]{64}):(\/.+)$/);
  if (match === null || match[2].endsWith("/") || match[2].includes("//")) {
    throw new RangeError("cert bundle reference is invalid");
  }

  return visible as CertBundleRef;
}

function subjectToken(value: string, label: string): string {
  if (value === "") {
    throw new RangeError(`${label} must not be empty`);
  }
  if (!/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new RangeError(`${label} must contain only ASCII letters, numbers, underscores, or dashes`);
  }

  return value;
}

function nonEmptyText(value: string, label: string): string {
  if (value.trim() === "") {
    throw new RangeError(`${label} must not be empty`);
  }

  return value;
}

function visibleAscii(value: string, label: string): string {
  if (value === "") {
    throw new RangeError(`${label} must not be empty`);
  }
  if (/[^\x21-\x7E]/.test(value)) {
    throw new RangeError(`${label} must contain only visible ASCII without whitespace`);
  }

  return value;
}

function positiveU16(value: number, label: string): number {
  if (!Number.isSafeInteger(value) || value < 1 || value > 65_535) {
    throw new RangeError(`${label} must be an integer from 1 to 65535`);
  }

  return value;
}

function positiveU64String(value: U64WireInput, label: string): string {
  const decimal = decimalIntegerString(value, label);
  const maxU64 = "18446744073709551615";
  if (decimal.length > maxU64.length || (decimal.length === maxU64.length && decimal > maxU64)) {
    throw new RangeError(`${label} must fit in an unsigned 64-bit integer`);
  }

  return decimal;
}

function decimalIntegerString(value: U64WireInput, label: string): string {
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || value < 1) {
      throw new RangeError(`${label} must be a positive safe integer when passed as a number`);
    }

    return String(value);
  }

  if (typeof value === "bigint") {
    if (value < 1n) {
      throw new RangeError(`${label} must be greater than zero`);
    }

    return value.toString();
  }

  if (!/^[1-9][0-9]*$/.test(value)) {
    throw new RangeError(`${label} must be a positive integer string`);
  }

  return value;
}
