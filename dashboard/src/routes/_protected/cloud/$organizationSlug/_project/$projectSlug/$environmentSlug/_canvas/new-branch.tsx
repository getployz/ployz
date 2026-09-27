import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote } from "#/collections/route-data";
import { deploymentAttemptQueryOptions } from "#/modules/deployments/deployment-history.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { NewBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/NewBranchPanel";

/**
 * "New branch of X": a panel over the Parent's canvas. `focus` names the lineage that changes; `fix` names the failed
 * attempt whose change to that lineage the Branch carries (Fix it on a branch).
 */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/new-branch",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ focus: Schema.optional(Schema.String), fix: Schema.optional(Schema.String) })),
  loaderDeps: ({ search }) => ({ fix: search.fix }),
  // The failed attempt names the change the Branch carries.
  loader: ({ params, context, deps }) => deps.fix ? prefetchRemote(context, deploymentAttemptQueryOptions(params.organizationSlug, deps.fix)) : undefined,
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="New branch" />,
  component: RouteComponent,
});

// The scene's BranchPickingProvider reads `focus`; the panel and the canvas share its picks.
function RouteComponent() {
  const { focus, fix } = Route.useSearch();
  return <NewBranchPanel focus={focus ?? null} fix={fix ?? null} />;
}
