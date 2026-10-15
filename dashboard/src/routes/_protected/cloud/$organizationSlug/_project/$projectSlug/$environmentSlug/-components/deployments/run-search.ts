import { linkOptions } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_LIVE_NODE_ROUTE_TO,
  ENVIRONMENT_RESOURCE_ROUTE_TO,
  ENVIRONMENT_SERVICE_ROUTE_TO,
  type EnvironmentRouteParams,
} from "../environment-route-paths";

export const DEPLOYMENT_ROUTE_TO = "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/$deploymentId";
export const VERSIONS_ROUTE_TO = "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/versions";

/** The Architecture node a run was opened from. A Live Node goes by its name, the token its route takes. */
const ArchitectureNodeRef = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("service"), id: Schema.String }),
  Schema.Struct({ kind: Schema.Literal("volume"), id: Schema.String }),
  Schema.Struct({ kind: Schema.Literal("config"), id: Schema.String }),
  Schema.Struct({ kind: Schema.Literal("live"), name: Schema.String }),
  Schema.Struct({ kind: Schema.Literal("mirror"), id: Schema.String }),
]);
export type ArchitectureNodeRef = typeof ArchitectureNodeRef.Type;

/** Its own Environment, since a Live Node's panel crosses into the Environment that owns it. */
const RunOrigin = Schema.Struct({ projectSlug: Schema.String, environmentSlug: Schema.String, node: ArchitectureNodeRef });
export type RunOrigin = typeof RunOrigin.Type;

const dropInvalid = () => Effect.succeed(Option.none());

export const runSearchSchema = Schema.Struct({
  job: Schema.optional(Schema.String),
  origin: Schema.optional(RunOrigin.pipe(Schema.catchDecoding(dropInvalid))),
  logs: Schema.optional(Schema.Literals(["build", "deploy"]).pipe(Schema.catchDecoding(dropInvalid))),
});
export type RunSearch = typeof runSearchSchema.Type;
export type LogStage = NonNullable<RunSearch["logs"]>;

function environmentOf({ organizationSlug, projectSlug, environmentSlug }: EnvironmentRouteParams): EnvironmentRouteParams {
  return { organizationSlug, projectSlug, environmentSlug };
}

export function deploymentDestination(scope: EnvironmentRouteParams, deploymentId: string, search: RunSearch = {}) {
  return linkOptions({ to: DEPLOYMENT_ROUTE_TO, params: { ...environmentOf(scope), deploymentId }, search });
}

/** Where the Run page's back goes: its origin node, selected on Architecture, or bare Architecture without one. */
export function nodeDestination(organizationSlug: string, scope: EnvironmentRouteParams, origin: RunOrigin | undefined) {
  if (!origin) return linkOptions({ to: ENVIRONMENT_INDEX_ROUTE_TO, params: environmentOf(scope), search: {} });
  const params = { organizationSlug, projectSlug: origin.projectSlug, environmentSlug: origin.environmentSlug };
  const { node } = origin;
  switch (node.kind) {
    case "service":
      return linkOptions({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: node.id }, search: {} });
    case "volume":
    case "config":
    case "mirror":
      return linkOptions({ to: ENVIRONMENT_RESOURCE_ROUTE_TO, params: { ...params, resourceId: node.id }, search: {} });
    case "live":
      return linkOptions({ to: ENVIRONMENT_LIVE_NODE_ROUTE_TO, params: { ...params, lineageId: node.name }, search: {} });
    default:
      return node satisfies never;
  }
}

/** A run opened from a node in this Environment. */
export function originFrom(scope: EnvironmentRouteParams, node: ArchitectureNodeRef): RunOrigin {
  return { projectSlug: scope.projectSlug, environmentSlug: scope.environmentSlug, node };
}
