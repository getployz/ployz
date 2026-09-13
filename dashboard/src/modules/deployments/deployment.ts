import { Schema } from "effect";
import { finiteNumber } from "#/modules/environment-design/schema";

const identity = Schema.Trim.check(Schema.isNonEmpty());
const positiveInteger = finiteNumber({ integer: true, minimum: 1 });
const machineId = Schema.String.check(
  Schema.isPattern(/^[0-9a-f]{32}$/),
);

export const DeploymentTriggerOrigin = Schema.Union([
  Schema.Struct({
    origin: Schema.Literal("manual"),
    actorId: identity,
  }),
  Schema.Struct({
    origin: Schema.Literal("github"),
    deliveryId: identity,
    branchEvaluationRevision: positiveInteger,
    installationId: positiveInteger,
    repositoryId: positiveInteger,
  }),
  Schema.Struct({
    origin: Schema.Literal("first_connect"),
    machineId,
  }),
]);

export type DeploymentTriggerOrigin = typeof DeploymentTriggerOrigin.Type;
