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
  type EventSequence,
  type FailureMessage,
  type ImageReference,
  type LogsTailLines,
  type MachineBootstrapUrl,
  type MachineJoinToken,
  type MachineName,
  type NodeId,
  type OperationEventReplayLimit,
  type OperationId,
  type OperationIdempotencyKey,
  type OperationLeaseExpiresAt,
  type OperationOwnerId,
  type OperatorHint,
  type ReplicaCount,
  type RevisionId,
  type RouteHostname,
  type RoutePort,
  type ServiceId,
} from "./generated.ts";

type U64WireInput = number | string | bigint;

export function operationId(value: string): OperationId {
  return subjectToken(value, "operation id") as OperationId;
}

export function operationOwnerId(value: string): OperationOwnerId {
  return subjectToken(value, "operation owner id") as OperationOwnerId;
}

export function operationIdempotencyKey(value: string): OperationIdempotencyKey {
  return subjectToken(value, "operation idempotency key") as OperationIdempotencyKey;
}

export function serviceId(value: string): ServiceId {
  return subjectToken(value, "service id") as ServiceId;
}

export function revisionId(value: string): RevisionId {
  return subjectToken(value, "revision id") as RevisionId;
}

export function nodeId(value: string): NodeId {
  return subjectToken(value, "node id") as NodeId;
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

export function operationLeaseExpiresAt(value: U64WireInput): OperationLeaseExpiresAt {
  return positiveU64String(value, "operation lease expiry") as OperationLeaseExpiresAt;
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
  const path = visible.startsWith("obj://") ? visible.slice("obj://".length) : "";
  const slash = path.indexOf("/");
  const bucket = slash >= 0 ? path.slice(0, slash) : "";
  const object = slash >= 0 ? path.slice(slash + 1) : "";
  if (bucket === "" || object === "" || object.startsWith("/") || object.includes("//")) {
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
