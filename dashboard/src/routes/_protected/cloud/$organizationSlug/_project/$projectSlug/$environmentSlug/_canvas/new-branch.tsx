import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { NewBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/NewBranchPanel";

/** "New branch of X": a panel over the Parent's canvas. `focus` names the lineage that changes. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/new-branch",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ focus: Schema.optional(Schema.String) })),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="New branch" />,
  component: RouteComponent,
});

// The scene's BranchPickingProvider reads `focus`; the panel and the canvas share its picks.
function RouteComponent() {
  return <NewBranchPanel />;
}
