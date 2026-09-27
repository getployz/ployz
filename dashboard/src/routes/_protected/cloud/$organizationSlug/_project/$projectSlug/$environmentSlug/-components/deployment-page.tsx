import { createContext, use, useEffect, useRef } from "react";
import { useLoaderData, useParams, type ParsedLocation } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { openStartedDeploymentsChange, setOpenStartedDeployments } from "#/auth/open-started-deployments";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { useDeploymentAttempt, type ViewedAttempt } from "#/modules/deployments/deployment.collection";
import { deploymentLighting, type DeploymentNodeView, type LogTab } from "#/modules/deployments/deployment-view";
import { Uuid } from "#/modules/environment-design/schema";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";

export const DEPLOYMENT_LIST_ROUTE_TO = "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments";
export const DEPLOYMENT_PAGE_ROUTE_TO =
  "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/$deploymentId";

/**
 * The Deployment Page's search: the focused service, the log tab the user picked, and the service whose Deployments tab
 * opened the page (closing returns there).
 */
export const deploymentPageSearchSchema = Schema.Struct({
  service: Schema.optional(Schema.String),
  returnTo: Schema.optional(Schema.String),
  logs: Schema.optional(Schema.Literals(["build", "deploy"]).pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
});

const legacyLink = Schema.Struct({ deployment: Uuid });
const legacyTab = Schema.Struct({ tab: Schema.Literals(["build-logs", "deploy-logs"]) });

/**
 * Where an old Deployment Mode link (`?deployment=<id>` on the canvas or on a service) now leads: that attempt's
 * Deployment Page, focused on the service and log tab it named. Null when the location carries no such link.
 */
export function legacyDeploymentLink({ pathname, searchStr }: Pick<ParsedLocation, "pathname" | "searchStr">) {
  // The raw string: the canvas route's search schema no longer knows `deployment`.
  const search = Object.fromEntries(new URLSearchParams(searchStr));
  if (!Schema.is(legacyLink)(search)) return null;
  const service = /\/services\/([^/]+)\/?$/.exec(pathname)?.[1];
  const logs: LogTab | undefined = Schema.is(legacyTab)(search) ? (search.tab === "build-logs" ? "build" : "deploy") : undefined;
  return { deploymentId: search.deployment, search: { service, logs } };
}

/** Which canvas nodes an open Deployment Page lights, by Node Outcome; null when no page is open. */
type Lighting = { lit: ReadonlyMap<string, DeploymentNodeView["outcome"]>; pending: boolean } | null;
const LightingContext = createContext<Lighting>(null);
export const DeploymentLightingProvider = LightingContext;

/** The canvas nodes an open Deployment Page lights; null when no page is open. */
export function useLitNodeIds() {
  const lighting = use(LightingContext);
  return lighting ? [...lighting.lit.keys()] : null;
}

/** A canvas node under an open Deployment Page: its outcome when the attempt changed it, `null` to dim it; `undefined` when no page is open. */
export function useNodeLighting(nodeId: string) {
  const lighting = use(LightingContext);
  if (!lighting) return undefined;
  const outcome = lighting.lit.get(nodeId);
  return outcome === undefined ? null : { outcome, pending: lighting.pending };
}

/**
 * The attempt whose Deployment Page is open, for the scene that stays mounted under it: the canvas lighting, and keeping
 * "open deployments I start" in step with how the user watches their own running attempts.
 */
export function useOpenDeployment(): Lighting {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const matched = useCanvasInspectorSelection().deploymentId;
  // A malformed id is no attempt; the page itself says so.
  const deploymentId = matched !== null && Schema.is(Uuid)(matched) ? matched : null;
  const { attempt } = useDeploymentAttempt(organizationSlug, environmentId, deploymentId, { buildLog: true });
  useOpenStartedDeploymentsSync(attempt, deploymentId);
  if (!attempt) return null;
  // Nodes the canvas no longer draws are simply never matched; the page lists them.
  return { lit: deploymentLighting(attempt), pending: attempt.buildPending };
}

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
