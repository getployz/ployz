import { Result } from "effect";
import {
  parseTypedDeployFailureEvidence,
  type TypedDeployFailureEvidence,
} from "#/modules/operations/deploy-operation-evidence";

export function managedNetworkingProgressLabel(stage: string) {
  switch (stage) {
    case "ensuring_certificates":
      return "Provisioning HTTPS certificates";
    case "route_cutover":
      return "Publishing routes to gateways";
    default:
      return null;
  }
}

export function managedNetworkingFailureLabel<T>(input: T) {
  const parsed = parseTypedDeployFailureEvidence(input);
  return Result.isSuccess(parsed) ? typedFailureLabel(parsed.success) : null;
}

function typedFailureLabel(failure: TypedDeployFailureEvidence) {
  switch (failure.kind) {
    case "automatic_hostname_collision":
      return `Automatic hostname ${failure.hostname} is already bound`;
    case "certificate_provision_timed_out":
      return `HTTPS certificate timed out for ${failure.hostname}`;
    case "certificate_provision_failed":
      return certificateFailureLabel(failure);
    case "route_cutover_failed":
      return routeCutoverFailureLabel(failure);
  }
}

function certificateFailureLabel(
  failure: Extract<
    TypedDeployFailureEvidence,
    { kind: "certificate_provision_failed" }
  >,
) {
  switch (failure.failure.class) {
    case "challenge_readiness":
      return `HTTPS challenge is not ready on ${failure.failure.missingMachineIds.length} ${failure.failure.missingMachineIds.length === 1 ? "server" : "servers"}`;
    case "gateway_artifact_push":
      return `HTTPS certificate could not reach server ${failure.failure.machineId}`;
    case "dns_preflight":
      return `DNS preflight failed for ${failure.hostname}`;
    case "challenge_publish":
      return `HTTPS challenge could not be published for ${failure.hostname}`;
    case "acme_validation":
      return `HTTPS validation failed for ${failure.hostname}`;
    case "core_interrupted":
      return `HTTPS provisioning was interrupted for ${failure.hostname}`;
    case "operation_evidence_write":
    case "active_cert_commit":
      return `HTTPS certificate could not be committed for ${failure.hostname}`;
  }
}

function routeCutoverFailureLabel(
  failure: Extract<TypedDeployFailureEvidence, { kind: "route_cutover_failed" }>,
) {
  switch (failure.reason.reason) {
    case "gateway_unavailable":
      return `Route could not reach gateway ${failure.reason.machineId}`;
    case "timed_out":
      return `Route publication timed out for ${failure.hostname}`;
    case "route_rejected":
      return `Route was rejected for ${failure.hostname}`;
    case "state_store_failed":
      return `Route state could not be committed for ${failure.hostname}`;
  }
}
