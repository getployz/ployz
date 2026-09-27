import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { NewBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/NewBranchPanel";

/** "New branch of X": a panel over the Parent's canvas. `focus` names the lineages that change. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/new-branch",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ focus: Schema.optional(Schema.Array(Schema.String)) })),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="New branch" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <NewBranchPanel focus={Route.useSearch().focus ?? []} />;
}
