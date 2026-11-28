import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

export const DeploymentTrigger = Schema.Literals(["github", "user"]);
export type DeploymentTrigger = typeof DeploymentTrigger.Type;

/** A Config Store Deployment was admitted; Cloud's worker runs it. */
export const configDeploymentAdmitted = defineEvent({
  name: "config/deployment.admitted",
  schema: Schema.Struct({
    organizationId: Schema.String,
    /** Deployments of one Environment run one at a time. */
    environmentId: Schema.String,
    deploymentId: Schema.String,
    trigger: Schema.optionalKey(DeploymentTrigger),
  }),
  id: ({ deploymentId }) => `config-deployment-admitted-${deploymentId}`,
});
export type ConfigDeploymentAdmittedEventData = typeof configDeploymentAdmitted.schema.Type;

/** Deploy now: unkeyed, so it asks again for a queued Deployment its admission's event never ran. */
export const createConfigDeploymentStartedEvent = (data: ConfigDeploymentAdmittedEventData) =>
  ({ name: configDeploymentAdmitted.name, data }) as const;

/**
 * A worker run of a Deployment stopped without finishing, and the Store didn't answer its stop: Cloud tries again until
 * it does. `round` counts the hand-offs, each a fresh run of tries.
 */
export const configDeploymentStopRequested = defineEvent({
  name: "config/deployment.stop-requested",
  schema: Schema.Struct({
    organizationId: Schema.String,
    deploymentId: Schema.String,
    runId: Schema.String,
    round: Schema.Number,
    /** The sweep's window that found the run's stop lost, so each window hands it on once. */
    sweep: Schema.optional(Schema.Number),
  }),
  id: ({ deploymentId, runId, round, sweep }) =>
    `config-deployment-stop-requested-${deploymentId}-${runId}-${sweep === undefined ? round : `sweep-${sweep}-${round}`}`,
});

/** A Config Store write named a pull request whose check Cloud publishes again from the Store's view. */
export const configPrCheckRequested = defineEvent({
  name: "config/pr-check.requested",
  schema: Schema.Struct({
    organizationId: Schema.String,
    repositoryId: Schema.Number,
    number: Schema.Number,
    /** `repositoryId:number`: one pull request's posts run one at a time. */
    pullRequestKey: Schema.String,
  }),
});

export const createConfigPrCheckRequestedEvent = (input: { organizationId: string; repositoryId: number; number: number }) =>
  configPrCheckRequested.create({ ...input, pullRequestKey: `${input.repositoryId}:${input.number}` });

/** The Organization's first Server joined: its published Environments deploy to it. */
export const configFirstServerJoined = defineEvent({
  name: "config/first-server.joined",
  schema: Schema.Struct({
    organizationId: Schema.String,
    /** The founding Server: a later founding (after teardown) is its own event. */
    machineId: Schema.String,
  }),
  id: ({ organizationId, machineId }) => `config-first-server-joined-${organizationId}-${machineId}`,
});

/** A Config Store write closed a Branch at once (nothing on a Server): its Store sweeps it away now. */
export const configSweepRequested = defineEvent({
  name: "config/sweep.requested",
  schema: Schema.Struct({ organizationId: Schema.String }),
});

export type ConfigPrCheckRequestedEventData = typeof configPrCheckRequested.schema.Type;
