import { createFileRoute } from "@tanstack/react-router";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { StoreTemplatePanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/StoreTemplatePanel";

/** A template running here, opened from its canvas node: its release, its Services, and what was changed away from it. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/templates/$templateId",
)({
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Template" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StoreTemplatePanel id={Route.useParams().templateId} />;
}
