import { createContext, use, useEffect, useRef, type ReactNode } from "react";
import { Link, retainSearchParams, useLoaderData, useParams, useSearch } from "@tanstack/react-router";
import { Schema } from "effect";
import { openStartedDeploymentsChange, setOpenStartedDeployments } from "#/auth/open-started-deployments";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { useDeploymentAttempt, type ViewedAttempt } from "#/modules/deployments/deployment.collection";
import { Uuid } from "#/modules/environment-design/schema";
import { DEPLOYMENT_SEARCH_KEY, ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

export const CANVAS_ROUTE_ID =
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas";

/**
 * The canvas route's search: `deployment=<id>` puts the canvas into Deployment Mode for that
 * Cloud Deployment Attempt and is retained across navigations inside the canvas.
 * Removing it (Back to editor, browser Back) returns to Editor Mode.
 * `deploymentList=true` opens the deploy bar's deployment list; it is not retained.
 */
export const canvasRouteSearch = {
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    deployment: Schema.optional(Schema.String),
    deploymentList: Schema.optional(Schema.Boolean),
  })),
  search: { middlewares: [retainSearchParams<{ deployment?: string }>([DEPLOYMENT_SEARCH_KEY])] },
};

/** A malformed id is no attempt: the canvas stays in Editor Mode. */
export const viewedDeploymentId = (search: { deployment?: string }) =>
  search.deployment !== undefined && Schema.is(Uuid)(search.deployment) ? search.deployment : null;

/** `attempt` is null in Editor Mode; `pendingId` names an attempt whose read is still on its way. */
type DeploymentMode = { attempt: ViewedAttempt | null; pendingId: string | null };
const DeploymentModeContext = createContext<DeploymentMode>({ attempt: null, pendingId: null });

/** The attempt the canvas shows in Deployment Mode, or null in Editor Mode (and while it loads). Everything in Deployment Mode is read-only. */
export function useDeploymentMode() {
  return use(DeploymentModeContext).attempt;
}

/** The attempt Deployment Mode is opening: its read (outside the Org Store) is on its way, so only the canvas nodes wait. */
export function usePendingDeploymentId() {
  return use(DeploymentModeContext).pendingId;
}

// ponytail: a malformed, unknown or other-environment id falls back to Editor Mode with the param still in the URL.
export function DeploymentModeProvider({ children }: { children: ReactNode }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const deploymentId = useSearch({ from: CANVAS_ROUTE_ID, select: viewedDeploymentId });
  const { attempt, pending } = useDeploymentAttempt(organizationSlug, environmentId, deploymentId, { buildLog: true });
  useOpenStartedDeploymentsSync(attempt, deploymentId);
  return <DeploymentModeContext value={{ attempt, pendingId: pending ? deploymentId : null }}>{children}</DeploymentModeContext>;
}

/** Keeps "open deployments I start" in step with how the user watches their own running attempts. */
function useOpenStartedDeploymentsSync(attempt: ViewedAttempt | null, deploymentId: string | null) {
  const { userId } = useCollectionScope();
  const origin = attempt?.deployment.triggerOrigin;
  const shownNow = attempt && isActiveDeployment(attempt.deployment.status)
    && origin?.origin === "manual" && origin.actorId === userId ? attempt.deployment.id : null;
  const shown = useRef<string | null>(null);
  useEffect(() => {
    const change = openStartedDeploymentsChange({ shownBefore: shown.current, shownNow, deploymentId });
    shown.current = shownNow;
    if (change !== null) void setOpenStartedDeployments(change);
  }, [shownNow, deploymentId]);
}

export function BackToLive({ className }: { className?: string }) {
  return (
    <Button
      size="sm"
      className={className}
      nativeButton={false}
      render={<Link to="." search={(previous) => ({ ...previous, deployment: undefined })} />}
    >
      Back to editor
    </Button>
  );
}
