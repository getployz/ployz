import { Data, Effect, Schema } from "effect";
import { strictParseOptions } from "#/modules/environment-design/schema";

const NonnegativeInt = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0));
const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const operationIdPayload = { operationId: Schema.String };
const automaticHostnameCollisionEvidenceSchema = Schema.Struct({
  kind: Schema.Literal("automatic_hostname_collision"),
  hostname: Schema.String,
  routeBindingId: Schema.String,
});
const certificateProvisionFailureEvidenceSchema = Schema.Union([
  Schema.Struct({ class: Schema.Literal("operation_evidence_write") }),
  Schema.Struct({ class: Schema.Literal("dns_preflight") }),
  Schema.Struct({ class: Schema.Literal("challenge_publish") }),
  Schema.Struct({
    class: Schema.Literal("challenge_readiness"),
    missingMachineIds: Schema.Array(Schema.String),
  }),
  Schema.Struct({ class: Schema.Literal("acme_validation") }),
  Schema.Struct({
    class: Schema.Literal("core_interrupted"),
    cause: Schema.Literals(["core_shutdown", "prior_core_process_loss"]),
    lastDurableStage: Schema.Union([
      Schema.Struct({ state: Schema.Literal("accepted") }),
      Schema.Struct({
        state: Schema.Literal("running"),
        stage: Schema.Literals(["challenge_published", "validation_started"]),
      }),
    ]),
    nextAction: Schema.Literal("retry_from_current_intent"),
  }),
  Schema.Struct({
    class: Schema.Literal("gateway_artifact_push"),
    machineId: Schema.String,
  }),
  Schema.Struct({ class: Schema.Literal("active_cert_commit") }),
]);
const certificateProvisionFailedEvidenceSchema = Schema.Struct({
  kind: Schema.Literal("certificate_provision_failed"),
  hostname: Schema.String,
  namespaceRevisionId: Schema.String,
  failure: certificateProvisionFailureEvidenceSchema,
});
const certificateProvisionTimedOutEvidenceSchema = Schema.Struct({
  kind: Schema.Literal("certificate_provision_timed_out"),
  hostname: Schema.String,
  namespaceRevisionId: Schema.String,
  timeoutSeconds: NonnegativeInt,
});
const routeCutoverReasonEvidenceSchema = Schema.Union([
  Schema.Struct({
    reason: Schema.Literal("gateway_unavailable"),
    machineId: Schema.String,
  }),
  Schema.Struct({ reason: Schema.Literal("route_rejected") }),
  Schema.Struct({ reason: Schema.Literal("state_store_failed") }),
  Schema.Struct({
    reason: Schema.Literal("timed_out"),
    timeoutSeconds: NonnegativeInt,
  }),
]);
const routeCutoverFailedEvidenceSchema = Schema.Struct({
  kind: Schema.Literal("route_cutover_failed"),
  hostname: Schema.String,
  reason: routeCutoverReasonEvidenceSchema,
});
const storageUnavailableReasonEvidenceSchema = Schema.Union([
  Schema.Struct({ reason: Schema.Literal("zfs_module_missing") }),
  Schema.Struct({
    reason: Schema.Literal("pool_not_imported"),
    pool: Schema.String,
  }),
  Schema.Struct({ reason: Schema.Literal("pool_faulted"), pool: Schema.String }),
  Schema.Struct({ reason: Schema.Literal("capacity_facts_unavailable") }),
]);
const storagePlacementReasonEvidenceSchema = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("storage_unprepared") }),
  Schema.Struct({ kind: Schema.Literal("storage_testimony_not_reported") }),
  Schema.Struct({
    kind: Schema.Literal("storage_unavailable"),
    reason: storageUnavailableReasonEvidenceSchema,
  }),
]);
const noUsableMachinesEvidenceSchema = Schema.Struct({
  kind: Schema.Literal("no_usable_machines"),
  reasons: Schema.Array(
    Schema.Struct({
      machineId: Schema.String,
      reason: storagePlacementReasonEvidenceSchema,
    }),
  ),
});
const typedDeployFailureEvidenceSchema = Schema.Union([
  noUsableMachinesEvidenceSchema,
  automaticHostnameCollisionEvidenceSchema,
  certificateProvisionFailedEvidenceSchema,
  certificateProvisionTimedOutEvidenceSchema,
  routeCutoverFailedEvidenceSchema,
]);
const failureSchema = Schema.Union([
  typedDeployFailureEvidenceSchema,
  Schema.Struct({ kind: Schema.String }),
]);
const phaseFailureSchema = Schema.Union([
  typedDeployFailureEvidenceSchema,
  Schema.Struct({ kind: NonEmptyString }),
]);
const runtimePhaseServiceSchema = Schema.Union([
  Schema.Struct({
    result: Schema.Literal("failed"),
    serviceId: NonEmptyString,
    failure: phaseFailureSchema,
  }),
  Schema.Struct({
    result: Schema.Literals(["completed", "skipped", "unchanged", "removed"]),
    serviceId: NonEmptyString,
  }),
]);
const deployPhaseFinishedEvidenceSchema = Schema.Struct({
  eventType: Schema.Literal("deploy_phase_finished"),
  payload: Schema.Struct({
    operationId: NonEmptyString,
    phase: NonnegativeInt,
    outcome: NonEmptyString,
    services: Schema.Array(runtimePhaseServiceSchema),
  }),
});
const deployOperationEvidenceSchema = Schema.Union([
  Schema.Struct({
    eventType: Schema.Literal("deploy_submitted"),
    payload: Schema.Struct({
      ...operationIdPayload,
      namespaceId: Schema.String,
      serviceCount: NonnegativeInt,
      services: Schema.Array(
        Schema.Struct({
          serviceId: Schema.String,
          environmentKeys: Schema.Array(Schema.String),
        }),
      ),
    }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_planning_started"),
    payload: Schema.Struct(operationIdPayload),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_image_resolved"),
    payload: Schema.Struct({
      ...operationIdPayload,
      serviceId: Schema.String,
      machineId: Schema.String,
      requested: Schema.String,
      resolved: Schema.String,
      credentialSupplied: Schema.Boolean,
    }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_plan_created"),
    payload: Schema.Struct({
      ...operationIdPayload,
      namespaceId: Schema.String,
      phaseCount: NonnegativeInt,
      serviceCount: NonnegativeInt,
    }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_running"),
    payload: Schema.Struct({ ...operationIdPayload, stage: Schema.String }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_image_availability_verified"),
    payload: Schema.Struct({
      ...operationIdPayload,
      serviceId: Schema.String,
      seed: Schema.String,
      manifestDigest: Schema.String,
    }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_container_started"),
    payload: Schema.Struct({
      ...operationIdPayload,
      machineId: Schema.String,
      containerId: Schema.String,
    }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_health_check_started"),
    payload: Schema.Struct(operationIdPayload),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_phase_started"),
    payload: Schema.Struct({
      ...operationIdPayload,
      phase: Schema.Int,
      serviceIds: Schema.Array(Schema.String),
    }),
  }),
  deployPhaseFinishedEvidenceSchema,
  Schema.Struct({
    eventType: Schema.Literal("deploy_cleanup_finished"),
    payload: Schema.Struct({
      ...operationIdPayload,
      removedCount: NonnegativeInt,
      failedCount: NonnegativeInt,
    }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_completed"),
    payload: Schema.Struct({
      ...operationIdPayload,
      outcome: Schema.Literals([
        "completed",
        "completed_with_warnings",
        "partially_completed",
        "partially_completed_with_warnings",
      ]),
    }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("deploy_failed"),
    payload: Schema.Struct({ ...operationIdPayload, failure: failureSchema }),
  }),
  Schema.Struct({
    eventType: Schema.Literal("cancelled"),
    payload: Schema.Struct({
      ...operationIdPayload,
      kind: Schema.Literal("deploy"),
      reason: Schema.Literal("[redacted]"),
    }),
  }),
]);

export type TypedDeployFailureEvidence = typeof typedDeployFailureEvidenceSchema.Type;
export type RuntimePhaseService = typeof runtimePhaseServiceSchema.Type;
export type RuntimePhaseEvidenceInput = {
  readonly eventType: string;
  readonly payload: object;
};

export class RuntimePhaseEvidenceInvalid extends Data.TaggedError(
  "RuntimePhaseEvidenceInvalid",
)<{ readonly message: string; readonly cause: unknown }> {}

export function parseDeployOperationEvidence(input: {
  eventType: string;
  payload: unknown;
}) {
  return Schema.decodeUnknownResult(deployOperationEvidenceSchema)(
    {
      eventType: input.eventType,
      payload: input.payload,
    },
    strictParseOptions,
  );
}

export function parseTypedDeployFailureEvidence<T>(input: T) {
  return Schema.decodeUnknownResult(typedDeployFailureEvidenceSchema)(
    input,
    strictParseOptions,
  );
}

export function decodeRuntimePhaseEvidence(value: RuntimePhaseEvidenceInput) {
  return Schema.decodeUnknownEffect(deployPhaseFinishedEvidenceSchema)(
    value,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      (cause) =>
        new RuntimePhaseEvidenceInvalid({
          message: "Persisted runtime phase evidence is invalid.",
          cause,
        }),
    ),
  );
}
