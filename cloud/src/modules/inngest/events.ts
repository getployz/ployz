import type { WebhookCustomerStateChangedPayload } from "@polar-sh/sdk/models/components/webhookcustomerstatechangedpayload";
import type { WebhookSubscriptionActivePayload } from "@polar-sh/sdk/models/components/webhooksubscriptionactivepayload";
import type { WebhookSubscriptionCanceledPayload } from "@polar-sh/sdk/models/components/webhooksubscriptioncanceledpayload";
import type { WebhookSubscriptionCreatedPayload } from "@polar-sh/sdk/models/components/webhooksubscriptioncreatedpayload";
import type { WebhookSubscriptionRevokedPayload } from "@polar-sh/sdk/models/components/webhooksubscriptionrevokedpayload";
import type { WebhookSubscriptionUncanceledPayload } from "@polar-sh/sdk/models/components/webhooksubscriptionuncanceledpayload";
import type { WebhookSubscriptionUpdatedPayload } from "@polar-sh/sdk/models/components/webhooksubscriptionupdatedpayload";
import { Schema } from "effect";
import { eventType, staticSchema } from "inngest";
import {
  githubCheckSuiteReceivedEventDataSchema,
  githubCheckSuiteTransitionEventDataSchema,
  githubEnvironmentTriggerPersistedEventDataSchema,
  githubPushReceivedEventDataSchema,
  type GithubCheckSuiteReceivedEventData,
  type GithubCheckSuiteReceivedEventInput,
  type GithubCheckSuiteTransitionEventData,
  type GithubEnvironmentTriggerPersistedEventData,
  type GithubPushReceivedEventData,
  type GithubPushReceivedEventInput,
} from "#/modules/github/github-ingestion.contracts";
import { asReferenceId } from "#/lib/json";
import type {
  GithubInstallationRepositoriesWebhook,
  GithubInstallationWebhook,
} from "#/modules/github/github-webhook-contracts";

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
  }),
});

export const githubInstallationReceivedEvent = "github/installation.received";
export const githubInstallationRepositoriesReceivedEvent =
  "github/installation-repositories.received";
export const githubRepositoriesSyncRequestedEvent =
  "github/repositories-sync.requested";
export const organizationBillingSyncRequestedEvent =
  "billing/organization-sync.requested";
export const environmentDeployRequestedEvent = "environment/deploy.requested";
export const githubEnvironmentTriggerPersistedEvent =
  "github/environment-trigger.persisted";
export const githubCheckSuiteTransitionedEvent =
  "github/check-suite.transitioned";
export const githubPushReceivedEvent = "github/push.received";
export const githubCheckSuiteReceivedEvent = "github/check-suite.received";
export const volumeRemoveRequestedEvent = "cloud/volume-remove.requested";
export const machineRemoveRequestedEvent = "machine/remove.requested";
export const teardownRequestedEvent = "cloud/teardown.requested";

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

export type EnvironmentDeployRequestedEventData = {
  environmentDeploymentId: string;
  environmentId: string;
};

export type MachineRemoveRequestedEventData = {
  attemptId: string;
};

export type VolumeRemoveRequestedEventData = {
  attemptId: string;
};

export type TeardownRequestedEventData = {
  attemptId: string;
};

export type InngestFunctionCancelledEventData = {
  function_id: string;
  run_id: string;
  correlation_id?: string;
};

export type {
  GithubCheckSuiteReceivedEventData,
  GithubCheckSuiteReceivedEventInput,
  GithubCheckSuiteTransitionEventData,
  GithubEnvironmentTriggerPersistedEventData,
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
export const environmentDeployRequestedEventType = eventType(
  environmentDeployRequestedEvent,
  { schema: staticSchema<EnvironmentDeployRequestedEventData>() },
);
export const githubPushReceivedEventType = eventType(
  githubPushReceivedEvent,
  { schema: staticSchema<GithubPushReceivedEventData>() },
);
export const githubCheckSuiteReceivedEventType = eventType(
  githubCheckSuiteReceivedEvent,
  { schema: staticSchema<GithubCheckSuiteReceivedEventData>() },
);
export const volumeRemoveRequestedEventType = eventType(
  volumeRemoveRequestedEvent,
  { schema: staticSchema<VolumeRemoveRequestedEventData>() },
);
export const machineRemoveRequestedEventType = eventType(
  machineRemoveRequestedEvent,
  { schema: staticSchema<MachineRemoveRequestedEventData>() },
);
export const teardownRequestedEventType = eventType(
  teardownRequestedEvent,
  { schema: staticSchema<TeardownRequestedEventData>() },
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

export function createVolumeRemoveRequestedEvent(
  data: VolumeRemoveRequestedEventData,
) {
  return {
    id: `volume-remove-${data.attemptId}`,
    name: volumeRemoveRequestedEvent,
    data,
  } as const;
}

export function createMachineRemoveRequestedEvent(
  data: MachineRemoveRequestedEventData,
) {
  return {
    id: `machine-remove-${data.attemptId}`,
    name: machineRemoveRequestedEvent,
    data,
  } as const;
}

export function createTeardownRequestedEvent(
  data: TeardownRequestedEventData,
) {
  return {
    id: `teardown-${data.attemptId}`,
    name: teardownRequestedEvent,
    data,
  } as const;
}

export function createOrganizationBillingSyncRequestedEvent(
  data: OrganizationBillingSyncRequestedEventData,
) {
  return {
    name: organizationBillingSyncRequestedEvent,
    data,
  } as const;
}

export function createEnvironmentDeployRequestedEvent(
  data: EnvironmentDeployRequestedEventData,
) {
  return {
    id: `environment-deploy-requested-${data.environmentDeploymentId}`,
    name: environmentDeployRequestedEvent,
    data,
  } as const;
}

export function createGithubEnvironmentTriggerPersistedEvent(
  data: GithubEnvironmentTriggerPersistedEventData,
) {
  const parsed = Schema.decodeUnknownSync(
    githubEnvironmentTriggerPersistedEventDataSchema,
  )(
    {
      ...data,
      serviceIds: Array.from(new Set(data.serviceIds)).sort((a, b) =>
        a.localeCompare(b),
      ),
    },
    { onExcessProperty: "error" },
  );
  const eventData = { ...parsed, serviceIds: Array.from(parsed.serviceIds) };
  return {
    id: `github-environment-trigger-${parsed.triggerId}-revision-${parsed.triggerRevision}`,
    name: githubEnvironmentTriggerPersistedEvent,
    data: eventData,
  } as const;
}

export function createGithubCheckSuiteTransitionEvent(
  data: GithubCheckSuiteTransitionEventData,
) {
  const parsed = Schema.decodeUnknownSync(
    githubCheckSuiteTransitionEventDataSchema,
  )(data, { onExcessProperty: "error" });
  return {
    id: `github-check-suite-${parsed.installationId}-${parsed.repositoryId}-${parsed.headSha}-${parsed.checkSuiteId}-revision-${parsed.transitionRevision}`,
    name: githubCheckSuiteTransitionedEvent,
    data: parsed,
  } as const;
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

type PolarSubscriptionWebhookPayload =
  | WebhookSubscriptionCreatedPayload
  | WebhookSubscriptionUpdatedPayload
  | WebhookSubscriptionActivePayload
  | WebhookSubscriptionCanceledPayload
  | WebhookSubscriptionRevokedPayload
  | WebhookSubscriptionUncanceledPayload;

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
      sourceUpdatedAt: payload.timestamp.toISOString(),
    }),
  ];
}

export function createOrganizationBillingSyncEventsFromCustomerStatePayload(
  payload: WebhookCustomerStateChangedPayload,
) {
  const organizationIds = Array.from(
    new Set(
      payload.data.activeSubscriptions
        .map(
          (
            subscription: WebhookCustomerStateChangedPayload["data"]["activeSubscriptions"][number],
          ) => coerceReferenceId(subscription.metadata["referenceId"]),
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
      sourceUpdatedAt: payload.timestamp.toISOString(),
    }),
  );
}

export type InngestSendableEvent =
  | ReturnType<typeof createGithubInstallationReceivedEvent>
  | ReturnType<typeof createGithubInstallationRepositoriesReceivedEvent>
  | ReturnType<typeof createGithubRepositoriesSyncRequestedEvent>
  | ReturnType<typeof createVolumeRemoveRequestedEvent>
  | ReturnType<typeof createMachineRemoveRequestedEvent>
  | ReturnType<typeof createTeardownRequestedEvent>
  | ReturnType<typeof createOrganizationBillingSyncRequestedEvent>
  | ReturnType<typeof createEnvironmentDeployRequestedEvent>
  | ReturnType<typeof createGithubEnvironmentTriggerPersistedEvent>
  | ReturnType<typeof createGithubCheckSuiteTransitionEvent>
  | ReturnType<typeof createGithubPushReceivedEvent>
  | ReturnType<typeof createGithubCheckSuiteReceivedEvent>;
