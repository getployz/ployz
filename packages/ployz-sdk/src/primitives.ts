import {
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
  type MachineId,
  type NamespaceId,
  type OperationEventReplayLimit,
  type OperationId,
  type OperationIdempotencyKey,
  type OperatorHint,
  type ReplicaCount,
  type RevisionId,
  type RouteHostname,
  type RoutePort,
  type ServiceId,
} from "./generated.ts";

type U64WireInput = number | string | bigint;

export function operationId(value: string): OperationId {
  return value as OperationId;
}

export function namespaceId(value: string): NamespaceId {
  return value as NamespaceId;
}

export function operationIdempotencyKey(value: string): OperationIdempotencyKey {
  return value as OperationIdempotencyKey;
}

export function serviceId(value: string): ServiceId {
  return value as ServiceId;
}

export function revisionId(value: string): RevisionId {
  return value as RevisionId;
}

export function machineId(value: string): MachineId {
  return value as MachineId;
}

export function machineName(value: string): MachineName {
  return value as MachineName;
}

export function machineBootstrapUrl(value: string): MachineBootstrapUrl {
  return value as MachineBootstrapUrl;
}

export function machineJoinToken(value: string): MachineJoinToken {
  return value as MachineJoinToken;
}

export function containerId(value: string): ContainerId {
  return value as ContainerId;
}

export function certId(value: string): CertId {
  return value as CertId;
}

export function imageReference(value: string): ImageReference {
  return value as ImageReference;
}

export function failureMessage(value: string): FailureMessage {
  return value as FailureMessage;
}

export function cancellationReason(value: string): CancellationReason {
  return value as CancellationReason;
}

export function operatorHint(value: string): OperatorHint {
  return value as OperatorHint;
}

export function replicaCount(value: number): ReplicaCount {
  return value as ReplicaCount;
}

export function eventSequence(value: U64WireInput): EventSequence {
  return String(value) as EventSequence;
}

export function certValidAt(value: U64WireInput): CertValidAt {
  return String(value) as CertValidAt;
}

export function acmeChallengeTtlSeconds(value: U64WireInput): AcmeChallengeTtlSeconds {
  return String(value) as AcmeChallengeTtlSeconds;
}

export function operationEventReplayLimit(
  value: number | OperationEventReplayLimit,
): OperationEventReplayLimit {
  return value as OperationEventReplayLimit;
}

export function logsTailLines(value: number | LogsTailLines): LogsTailLines {
  return value as LogsTailLines;
}

export function routeHostname(value: string): RouteHostname {
  return value as RouteHostname;
}

export function routePort(value: number): RoutePort {
  return value as RoutePort;
}

export function acmeChallengeToken(value: string): AcmeChallengeToken {
  return value as AcmeChallengeToken;
}

export function acmeChallengeValue(value: string): AcmeChallengeValue {
  return value as AcmeChallengeValue;
}

export function certBundleRef(value: string): CertBundleRef {
  return value as CertBundleRef;
}
