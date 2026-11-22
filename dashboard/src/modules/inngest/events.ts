import type { webhooks } from "#/modules/billing/polar-api";
import { Schema } from "effect";
import { eventType, staticSchema } from "inngest";
import type { createConfigDeploymentStartedEvent, createConfigPrCheckRequestedEvent, configDeploymentAdmitted, configDeploymentRetireRequested, configFirstServerJoined, configSweepRequested } from "#/modules/config-store/config-store.events";
import type { createClusterNameRequestedEvent } from "#/modules/cluster-name/cluster-name.events";
import type { clusterDomainSyncRequested } from "#/modules/cluster-domain/cluster-domain.events";
import type { ingressDoorsRequested } from "#/modules/ingress/ingress.events";
import type { createMailSendRequestedEvent } from "#/modules/mail/mail.events";
import type { createKeyedServerPolicyChangeRequestedEvent, machineRemoveRequested, namespaceCleanupRequested, serverAccessRetireRequested, serverDrainRequested, serverPolicyChangeRequested, serverWebTrafficRequested } from "#/modules/machines/machines.events";
import type { createServerEnrollmentCompletedEvent, createServerEnrollmentFoundedEvent, createServerEnrollmentReportedEvent, serverEnrollmentCancelled, serverEnrollmentRequested } from "#/modules/machines/waiting/server-enrollment.events";
import type { createCliServerUpgradeRequestedEvent, serverUpgradeRequested } from "#/modules/server-upgrade/server-upgrade.events";
import type { serviceActRequested } from "#/modules/service-act/service-act.events";
import type { volumeRunRequested, volumeRunTaken } from "#/modules/volume-run/volume-run.events";
import { githubCheckSuiteReceivedEventDataSchema, githubPullRequestReceivedEventDataSchema, githubPushReceivedEventDataSchema, type GithubCheckSuiteReceivedEventData, type GithubCheckSuiteReceivedEventInput, type GithubPullRequestReceivedEventData, type GithubPullRequestReceivedEventInput, type GithubPushReceivedEventData, type GithubPushReceivedEventInput } from "#/modules/github/github-ingestion.contracts";
import { asReferenceId } from "#/lib/json";
import type { GithubInstallationRepositoriesWebhook, GithubInstallationWebhook } from "#/modules/github/github-webhook-contracts";

export const inngestEventEnvelopeFields = {
  id: Schema.optionalKey(Schema.String),
  v: Schema.optionalKey(Schema.String),
  ts: Schema.optionalKey(Schema.Finite.check(Schema.isInt())),
};

export const inngestEventIdentitySchema = Schema.Trim.check(
  Schema.isMinLength(1),
);

export const inngestFailureErrorSchema = Schema.Struct({
  name: Schema.String,
  message: Schema.String,
});

export function inngestFunctionFailedEnvelopeSchema<
  EventSchema extends Schema.Constraint,
>(eventSchema: EventSchema) {
  return Schema.Struct({
    ...inngestEventEnvelopeFields,
    name: Schema.Literal("inngest/function.failed"),
    data: Schema.Struct({
      function_id: inngestEventIdentitySchema,
      run_id: inngestEventIdentitySchema,
      error: inngestFailureErrorSchema,
      event: eventSchema,
    }),
  });
}

export const inngestFunctionCancelledEnvelopeSchema = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal("inngest/function.cancelled"),
  data: Schema.Struct({
    function_id: inngestEventIdentitySchema,
    run_id: inngestEventIdentitySchema,
    correlation_id: Schema.optionalKey(Schema.String),
    /** The cancelled run's triggering event; each handler decodes what it needs from it. */
    event: Schema.optionalKey(Schema.Unknown),
  }),
});

export const inngestFunctionFinishedEnvelopeSchema = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal("inngest/function.finished"),
  data: Schema.Struct({
    function_id: inngestEventIdentitySchema,
    run_id: inngestEventIdentitySchema,
    error: Schema.optionalKey(Schema.NullOr(inngestFailureErrorSchema)),
    event: Schema.optionalKey(Schema.Unknown),
  }),
});

export const githubInstallationReceivedEvent = "github/installation.received";
export const githubInstallationRepositoriesReceivedEvent =
  "github/installation-repositories.received";
export const githubRepositoriesSyncRequestedEvent =
  "github/repositories-sync.requested";
export const organizationBillingSyncRequestedEvent =
  "billing/organization-sync.requested";
export const githubPushReceivedEvent = "github/push.received";
export const githubCheckSuiteReceivedEvent = "github/check-suite.received";
export const githubPullRequestReceivedEvent = "github/pull-request.received";
export const sealingKeyEnsureRequestedEvent = "sealing-key/ensure.requested";
export const variablesResealRequestedEvent = "variables/reseal.requested";
export const observeCrashReceivedEvent = "observe/crash.received";
export const observePushTargetRequestedEvent = "observe/push-target.requested";
export const anonymousTeardownRequestedEvent = "anonymous/teardown.requested";

export type GithubInstallationWebhookEventData = GithubInstallationWebhook & {
  deliveryId: string;
};

export type GithubInstallationRepositoriesWebhookEventData =
  GithubInstallationRepositoriesWebhook & { deliveryId: string };

export type GithubRepositoriesSyncRequestedEventData = {
  installationId: number;
  reason: string;
};

export type OrganizationBillingSyncRequestedEventData = {
  organizationId: string;
  reason: string;
  sourceUpdatedAt?: string;
};

export type SealingKeyEnsureRequestedEventData = {
  organizationId: string;
};

/** Reseal the Legacy secrets of `organizationId`, or of every Organization that holds one. */
export type VariablesResealRequestedEventData = {
  organizationId?: string;
};

export type ObserveCrashReceivedEventData = {
  organizationId: string;
  serviceId: string;
};

/** An anonymous agent's Organization is past its token: tear it down one step further. */
export type AnonymousTeardownRequestedEventData = {
  organizationId: string;
};

export type InngestFunctionCancelledEventData = {
  function_id: string;
  run_id: string;
  correlation_id?: string;
  event?: unknown;
};

export type {
  GithubCheckSuiteReceivedEventData,
  GithubCheckSuiteReceivedEventInput,
  GithubPullRequestReceivedEventData,
  GithubPullRequestReceivedEventInput,
  GithubPushReceivedEventData,
  GithubPushReceivedEventInput,
};

export const githubInstallationReceivedEventType = eventType(
  githubInstallationReceivedEvent,
  { schema: staticSchema<GithubInstallationWebhookEventData>() },
);
export const githubInstallationRepositoriesReceivedEventType = eventType(
  githubInstallationRepositoriesReceivedEvent,
  { schema: staticSchema<GithubInstallationRepositoriesWebhookEventData>() },
);
export const githubRepositoriesSyncRequestedEventType = eventType(
  githubRepositoriesSyncRequestedEvent,
  { schema: staticSchema<GithubRepositoriesSyncRequestedEventData>() },
);
export const organizationBillingSyncRequestedEventType = eventType(
  organizationBillingSyncRequestedEvent,
  { schema: staticSchema<OrganizationBillingSyncRequestedEventData>() },
);
export const observePushTargetRequestedEventType = eventType(
  observePushTargetRequestedEvent,
  { schema: staticSchema<Record<string, never>>() },
);
export const githubPushReceivedEventType = eventType(
  githubPushReceivedEvent,
  { schema: staticSchema<GithubPushReceivedEventData>() },
);
export const githubCheckSuiteReceivedEventType = eventType(
  githubCheckSuiteReceivedEvent,
  { schema: staticSchema<GithubCheckSuiteReceivedEventData>() },
);
export const githubPullRequestReceivedEventType = eventType(
  githubPullRequestReceivedEvent,
  { schema: staticSchema<GithubPullRequestReceivedEventData>() },
);
export const inngestFunctionFinishedEventType = eventType(
  "inngest/function.finished",
  { schema: staticSchema<{ function_id: string; run_id: string; error?: { name: string; message: string } | null }>() },
);
export const anonymousTeardownRequestedEventType = eventType(
  anonymousTeardownRequestedEvent,
  { schema: staticSchema<AnonymousTeardownRequestedEventData>() },
);
export const sealingKeyEnsureRequestedEventType = eventType(
  sealingKeyEnsureRequestedEvent,
  { schema: staticSchema<SealingKeyEnsureRequestedEventData>() },
);
export const variablesResealRequestedEventType = eventType(
  variablesResealRequestedEvent,
  { schema: staticSchema<VariablesResealRequestedEventData>() },
);
export const observeCrashReceivedEventType = eventType(
  observeCrashReceivedEvent,
  { schema: staticSchema<ObserveCrashReceivedEventData>() },
);
export const inngestFunctionCancelledEventType = eventType(
  "inngest/function.cancelled",
  {
    schema: staticSchema<InngestFunctionCancelledEventData>(),
  },
);

function coerceReferenceId<T>(value: T) {
  return asReferenceId(value);
}

export function createGithubInstallationReceivedEvent(
  data: GithubInstallationWebhookEventData,
) {
  return {
    id: data.deliveryId,
    name: githubInstallationReceivedEvent,
    data,
  } as const;
}

export function createGithubInstallationRepositoriesReceivedEvent(
  data: GithubInstallationRepositoriesWebhookEventData,
) {
  const eventData = {
    ...data,
    repositories_added: data.repositories_added.map((repository) => ({
      ...repository,
    })),
    repositories_removed: data.repositories_removed.map((repository) => ({
      ...repository,
    })),
  };
  return {
    id: data.deliveryId,
    name: githubInstallationRepositoriesReceivedEvent,
    data: eventData,
  } as const;
}

export function createGithubRepositoriesSyncRequestedEvent(
  data: GithubRepositoriesSyncRequestedEventData,
) {
  return {
    name: githubRepositoriesSyncRequestedEvent,
    data,
  } as const;
}

export function createAnonymousTeardownRequestedEvent(data: AnonymousTeardownRequestedEventData) {
  return { name: anonymousTeardownRequestedEvent, data } as const;
}

export function createObserveCrashReceivedEvent(data: ObserveCrashReceivedEventData, crashEventId: string) {
  return { id: `observe-crash-${crashEventId}`, name: observeCrashReceivedEvent, data } as const;
}

export function createSealingKeyEnsureRequestedEvent(data: SealingKeyEnsureRequestedEventData) {
  return { name: sealingKeyEnsureRequestedEvent, data } as const;
}

export function createVariablesResealRequestedEvent(data: VariablesResealRequestedEventData) {
  return { name: variablesResealRequestedEvent, data } as const;
}

/** Sent when a worker starts, so an install whose Clusters reported their keys before it reseals too; resealing again changes nothing. */
export function createStartupResealEvent(startedAt: Date) {
  return { id: `variables-reseal-start-${startedAt.getTime()}`, name: variablesResealRequestedEvent, data: {} } as const;
}

/** Keyed by the Cluster whose first Sealing Key Cloud recorded: a retried record sends it again, and Inngest runs it once. */
export function createClusterResealRequestedEvent({ organizationId, clusterKey }: { organizationId: string; clusterKey: string }) {
  return { id: `variables-reseal-${organizationId}-${clusterKey}`, name: variablesResealRequestedEvent, data: { organizationId } } as const;
}

export function createOrganizationBillingSyncRequestedEvent(
  data: OrganizationBillingSyncRequestedEventData,
) {
  return {
    name: organizationBillingSyncRequestedEvent,
    data,
  } as const;
}

/** A Machine's capability was published: tell it where to push. The 5-minute sweep covers a lost event. */
export function createObservePushTargetRequestedEvent() {
  return { name: observePushTargetRequestedEvent, data: {} satisfies Record<string, never> } as const;
}

export function createGithubPushReceivedEvent(
  input: GithubPushReceivedEventInput,
) {
  const data = Schema.decodeUnknownSync(githubPushReceivedEventDataSchema)(
    {
      ...input,
      branchKey: `${input.installationId}:${input.repositoryId}:${input.ref}`,
    },
    { onExcessProperty: "error" },
  );
  return {
    id: data.deliveryId,
    name: githubPushReceivedEvent,
    data,
  } as const;
}

export function createGithubCheckSuiteReceivedEvent(
  input: GithubCheckSuiteReceivedEventInput,
) {
  const data = Schema.decodeUnknownSync(
    githubCheckSuiteReceivedEventDataSchema,
  )(
    {
      ...input,
      checkSuiteKey: `${input.installationId}:${input.repositoryId}:${input.checkSuiteId}`,
    },
    { onExcessProperty: "error" },
  );
  return {
    id: data.deliveryId,
    name: githubCheckSuiteReceivedEvent,
    data,
  } as const;
}

export function createGithubPullRequestReceivedEvent(
  input: GithubPullRequestReceivedEventInput,
) {
  const data = Schema.decodeUnknownSync(
    githubPullRequestReceivedEventDataSchema,
  )(
    {
      ...input,
      pullRequestKey: `${input.installationId}:${input.repositoryId}:${input.number}`,
    },
    { onExcessProperty: "error" },
  );
  return {
    id: data.deliveryId,
    name: githubPullRequestReceivedEvent,
    data,
  } as const;
}

type PolarSubscriptionWebhookPayload =
  | webhooks.WebhookSubscriptionCreatedPayload
  | webhooks.WebhookSubscriptionUpdatedPayload
  | webhooks.WebhookSubscriptionActivePayload
  | webhooks.WebhookSubscriptionCanceledPayload
  | webhooks.WebhookSubscriptionRevokedPayload
  | webhooks.WebhookSubscriptionUncanceledPayload;

export function createOrganizationBillingSyncEventsFromSubscriptionPayload(
  payload: PolarSubscriptionWebhookPayload,
) {
  const organizationId = coerceReferenceId(
    payload.data.metadata["referenceId"],
  );

  if (!organizationId) {
    return [];
  }

  return [
    createOrganizationBillingSyncRequestedEvent({
      organizationId,
      reason: payload.type,
      sourceUpdatedAt: new Date(payload.timestamp).toISOString(),
    }),
  ];
}

export function createOrganizationBillingSyncEventsFromCustomerStatePayload(
  payload: webhooks.WebhookCustomerStateChangedPayload,
) {
  const organizationIds = Array.from(
    new Set(
      payload.data.active_subscriptions
        .map((subscription) =>
          coerceReferenceId(subscription.metadata["referenceId"]),
        )
        .filter(
          (organizationId): organizationId is string => organizationId !== null,
        ),
    ),
  );

  if (organizationIds.length === 0) {
    return [];
  }

  return organizationIds.map((organizationId) =>
    createOrganizationBillingSyncRequestedEvent({
      organizationId,
      reason: payload.type,
      sourceUpdatedAt: new Date(payload.timestamp).toISOString(),
    }),
  );
}

export type InngestSendableEvent =
  | ReturnType<typeof createGithubInstallationReceivedEvent>
  | ReturnType<typeof createGithubInstallationRepositoriesReceivedEvent>
  | ReturnType<typeof createGithubRepositoriesSyncRequestedEvent>
  | ReturnType<typeof machineRemoveRequested.create>
  | ReturnType<typeof serverPolicyChangeRequested.create>
  | ReturnType<typeof createKeyedServerPolicyChangeRequestedEvent>
  | ReturnType<typeof serverUpgradeRequested.create>
  | ReturnType<typeof createCliServerUpgradeRequestedEvent>
  | ReturnType<typeof serverDrainRequested.create>
  | ReturnType<typeof serverEnrollmentRequested.create>
  | ReturnType<typeof createServerEnrollmentReportedEvent>
  | ReturnType<typeof createServerEnrollmentCompletedEvent>
  | ReturnType<typeof serverEnrollmentCancelled.create>
  | ReturnType<typeof createServerEnrollmentFoundedEvent>
  | ReturnType<typeof serverWebTrafficRequested.create>
  | ReturnType<typeof ingressDoorsRequested.create>
  | ReturnType<typeof namespaceCleanupRequested.create>
  | ReturnType<typeof serviceActRequested.create>
  | ReturnType<typeof createMailSendRequestedEvent>
  | ReturnType<typeof volumeRunRequested.create>
  | ReturnType<typeof volumeRunTaken.create>
  | ReturnType<typeof clusterDomainSyncRequested.create>
  | ReturnType<typeof createClusterNameRequestedEvent>
  | ReturnType<typeof createSealingKeyEnsureRequestedEvent>
  | ReturnType<typeof createVariablesResealRequestedEvent>
  | ReturnType<typeof createClusterResealRequestedEvent>
  | ReturnType<typeof createStartupResealEvent>
  | ReturnType<typeof createObserveCrashReceivedEvent>
  | ReturnType<typeof configFirstServerJoined.create>
  | ReturnType<typeof createOrganizationBillingSyncRequestedEvent>
  | ReturnType<typeof configDeploymentAdmitted.create>
  | ReturnType<typeof serverAccessRetireRequested.create>
  | ReturnType<typeof createObservePushTargetRequestedEvent>
  | ReturnType<typeof createConfigPrCheckRequestedEvent>
  | ReturnType<typeof configSweepRequested.create>
  | ReturnType<typeof createConfigDeploymentStartedEvent>
  | ReturnType<typeof configDeploymentRetireRequested.create>
  | ReturnType<typeof createGithubPushReceivedEvent>
  | ReturnType<typeof createGithubCheckSuiteReceivedEvent>
  | ReturnType<typeof createGithubPullRequestReceivedEvent>
  | ReturnType<typeof createGithubBuildRunCompletedEvent>
  | ReturnType<typeof createAnonymousTeardownRequestedEvent>;

/**
 * A dispatched GitHub build run completed, whatever its conclusion, or its runner's final report
 * settled the build. The waiting Image Build checks it. `id` dedupes the send.
 */
export const githubBuildRunCompletedEvent = "github/build-run.completed";
export function createGithubBuildRunCompletedEvent(data: { id: string; runId: number }) {
  return { id: data.id, name: githubBuildRunCompletedEvent, data: { runId: data.runId } };
}
