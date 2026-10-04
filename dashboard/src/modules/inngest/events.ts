import type { webhooks } from "#/modules/billing/polar-api";
import { Schema } from "effect";
import type { ServerPolicyChange } from "#/modules/machines/server-policy";
import type { UpgradeTrigger } from "#/modules/server-upgrade/server-upgrade";
import { eventType, staticSchema } from "inngest";
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
export const machineRemoveRequestedEvent = "machine/remove.requested";
export const serverPolicyChangeRequestedEvent = "machine/policy-change.requested";
export const serverUpgradeRequestedEvent = "server/upgrade.requested";
export const clusterDomainSyncRequestedEvent = "cluster-domain/sync.requested";
export const configDeploymentAdmittedEvent = "config/deployment.admitted";
export const configPrCheckRequestedEvent = "config/pr-check.requested";
export const serverAccessRetireRequestedEvent = "server-access/retire.requested";
export const configFirstServerJoinedEvent = "config/first-server.joined";
export const configSweepRequestedEvent = "config/sweep.requested";

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

export type ServerPolicyChangeRequestedEventData = {
  organizationId: string;
  machineId: string;
  change: ServerPolicyChange;
};

/** A rollout request. A manual one names who clicked and the Server they clicked on. */
export type ServerUpgradeRequestedEventData = {
  organizationId: string;
  /** The Server to Upgrade; null upgrades every Server behind. */
  machineId: string | null;
  trigger: UpgradeTrigger;
  userId: string | null;
};

export type MachineRemoveRequestedEventData = {
  attemptId: string;
};

export type ClusterDomainSyncRequestedEventData = {
  organizationId: string;
};

/** The Organization's first Server joined: its published Environments deploy to it. */
export type ConfigFirstServerJoinedEventData = {
  organizationId: string;
  /** The founding Server: a later founding (after teardown) is its own event. */
  machineId: string;
};

/** A Config Store write closed a Branch at once (nothing on a Server): its Store sweeps it away now. */
export type ConfigSweepRequestedEventData = {
  organizationId: string;
};

/** A Config Store write named a pull request whose check Cloud publishes again from the Store's view. */
export type ConfigPrCheckRequestedEventData = {
  organizationId: string;
  repositoryId: number;
  number: number;
  /** `repositoryId:number`: one pull request's posts run one at a time. */
  pullRequestKey: string;
};

/** A member left an Organization (when the request named it): their devices' holders on its Servers go now. */
export type ServerAccessRetireRequestedEventData = Record<string, never>;

/** A Config Store Deployment was admitted; Cloud's worker runs it. */
export type ConfigDeploymentAdmittedEventData = {
  organizationId: string;
  /** Deployments of one Environment run one at a time. */
  environmentId: string;
  deploymentId: string;
};

export type InngestFunctionCancelledEventData = {
  function_id: string;
  run_id: string;
  correlation_id?: string;
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
export const configPrCheckRequestedEventType = eventType(
  configPrCheckRequestedEvent,
  { schema: staticSchema<ConfigPrCheckRequestedEventData>() },
);
export const serverAccessRetireRequestedEventType = eventType(
  serverAccessRetireRequestedEvent,
  { schema: staticSchema<ServerAccessRetireRequestedEventData>() },
);
export const configDeploymentAdmittedEventType = eventType(
  configDeploymentAdmittedEvent,
  { schema: staticSchema<ConfigDeploymentAdmittedEventData>() },
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
export const machineRemoveRequestedEventType = eventType(
  machineRemoveRequestedEvent,
  { schema: staticSchema<MachineRemoveRequestedEventData>() },
);
export const serverPolicyChangeRequestedEventType = eventType(
  serverPolicyChangeRequestedEvent,
  { schema: staticSchema<ServerPolicyChangeRequestedEventData>() },
);
export const serverUpgradeRequestedEventType = eventType(
  serverUpgradeRequestedEvent,
  { schema: staticSchema<ServerUpgradeRequestedEventData>() },
);
export const configFirstServerJoinedEventType = eventType(
  configFirstServerJoinedEvent,
  { schema: staticSchema<ConfigFirstServerJoinedEventData>() },
);
export const configSweepRequestedEventType = eventType(
  configSweepRequestedEvent,
  { schema: staticSchema<ConfigSweepRequestedEventData>() },
);
export const clusterDomainSyncRequestedEventType = eventType(
  clusterDomainSyncRequestedEvent,
  { schema: staticSchema<ClusterDomainSyncRequestedEventData>() },
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

export function createMachineRemoveRequestedEvent(
  data: MachineRemoveRequestedEventData,
) {
  return {
    id: `machine-remove-${data.attemptId}`,
    name: machineRemoveRequestedEvent,
    data,
  } as const;
}

export function createServerPolicyChangeRequestedEvent(
  data: ServerPolicyChangeRequestedEventData,
) {
  return { name: serverPolicyChangeRequestedEvent, data } as const;
}

export function createServerUpgradeRequestedEvent(data: ServerUpgradeRequestedEventData) {
  return { name: serverUpgradeRequestedEvent, data } as const;
}

/** Keyed by founding: a retried completion sends it again, and Inngest runs it once. */
export function createConfigFirstServerJoinedEvent(data: ConfigFirstServerJoinedEventData) {
  return { id: `config-first-server-joined-${data.organizationId}-${data.machineId}`, name: configFirstServerJoinedEvent, data } as const;
}

export function createConfigSweepRequestedEvent(data: ConfigSweepRequestedEventData) {
  return { name: configSweepRequestedEvent, data } as const;
}

export function createClusterDomainSyncRequestedEvent(
  data: ClusterDomainSyncRequestedEventData,
) {
  return { name: clusterDomainSyncRequestedEvent, data } as const;
}

export function createOrganizationBillingSyncRequestedEvent(
  data: OrganizationBillingSyncRequestedEventData,
) {
  return {
    name: organizationBillingSyncRequestedEvent,
    data,
  } as const;
}

/** Keyed by the Deployment, so admitting it again (a replayed request) sends nothing new. */
export function createConfigPrCheckRequestedEvent(input: { organizationId: string; repositoryId: number; number: number }) {
  return {
    name: configPrCheckRequestedEvent,
    data: { ...input, pullRequestKey: `${input.repositoryId}:${input.number}` } satisfies ConfigPrCheckRequestedEventData,
  } as const;
}

export function createServerAccessRetireRequestedEvent() {
  return { name: serverAccessRetireRequestedEvent, data: {} satisfies ServerAccessRetireRequestedEventData } as const;
}

export function createConfigDeploymentAdmittedEvent(data: ConfigDeploymentAdmittedEventData) {
  return { id: `config-deployment-admitted-${data.deploymentId}`, name: configDeploymentAdmittedEvent, data } as const;
}

/** Deploy now: unkeyed, so it asks again for a queued Deployment its admission's event never ran. */
export function createConfigDeploymentStartedEvent(data: ConfigDeploymentAdmittedEventData) {
  return { name: configDeploymentAdmittedEvent, data } as const;
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
  | ReturnType<typeof createMachineRemoveRequestedEvent>
  | ReturnType<typeof createServerPolicyChangeRequestedEvent>
  | ReturnType<typeof createServerUpgradeRequestedEvent>
  | ReturnType<typeof createClusterDomainSyncRequestedEvent>
  | ReturnType<typeof createConfigFirstServerJoinedEvent>
  | ReturnType<typeof createOrganizationBillingSyncRequestedEvent>
  | ReturnType<typeof createConfigDeploymentAdmittedEvent>
  | ReturnType<typeof createServerAccessRetireRequestedEvent>
  | ReturnType<typeof createConfigPrCheckRequestedEvent>
  | ReturnType<typeof createConfigSweepRequestedEvent>
  | ReturnType<typeof createConfigDeploymentStartedEvent>
  | ReturnType<typeof createGithubPushReceivedEvent>
  | ReturnType<typeof createGithubCheckSuiteReceivedEvent>
  | ReturnType<typeof createGithubPullRequestReceivedEvent>
  | ReturnType<typeof createGithubBuildRunCompletedEvent>;

/**
 * A dispatched GitHub build run completed, whatever its conclusion, or its runner's final report
 * settled the build. The waiting Image Build checks it. `id` dedupes the send.
 */
export const githubBuildRunCompletedEvent = "github/build-run.completed";
export function createGithubBuildRunCompletedEvent(data: { id: string; runId: number }) {
  return { id: data.id, name: githubBuildRunCompletedEvent, data: { runId: data.runId } };
}
