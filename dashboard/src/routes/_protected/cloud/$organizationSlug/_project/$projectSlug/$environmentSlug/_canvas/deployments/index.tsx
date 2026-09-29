import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { DeploymentsList } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/DeploymentsList";

/**
 * The Environment's Deployments, newest first: a panel over the live canvas. `service` narrows it to one. The
 * Environment's loader prefetched their first page for the bottom bar, and this list is that read.
 */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ service: Schema.optional(Schema.String) })),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Deployments" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <DeploymentsList service={Route.useSearch().service ?? null} />;
}
