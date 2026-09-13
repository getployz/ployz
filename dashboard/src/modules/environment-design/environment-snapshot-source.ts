import { Schema } from "effect";
import { ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/tables";
import { Uuid } from "#/modules/environment-design/workspace-schemas";

export const environmentSnapshotSourceSchema = Schema.Union([
  Schema.Struct({
    kind: Schema.Literal("saved"),
    environmentSavedStateSnapshotId: Uuid,
  }),
  Schema.Struct({
    kind: Schema.Literal("deployment"),
    environmentDeploymentId: Uuid,
    status: Schema.Literals(ENVIRONMENT_DEPLOYMENT_STATUSES),
  }),
]);

export type EnvironmentSnapshotSource =
  typeof environmentSnapshotSourceSchema.Type;
