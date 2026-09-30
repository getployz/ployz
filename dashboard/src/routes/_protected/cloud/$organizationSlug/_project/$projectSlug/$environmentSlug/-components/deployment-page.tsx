import { createContext, use } from "react";
import { useParams } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { deploymentQuery, useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { nodeLight, nodeOutcomeLabel, type NodeLight } from "#/modules/config-store/store-deployments";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";
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

/** One node an open Deployment Page lights: how its outcome shows, and its words. */
export type Lit = { outcome: NodeLight; label: string };
/** Which canvas nodes an open Deployment Page (`deploymentId`) lights, by Node Outcome; null when no page is open. */
type Lighting = { deploymentId: string; lit: ReadonlyMap<string, Lit> } | null;
const LightingContext = createContext<Lighting>(null);
export const DeploymentLightingProvider = LightingContext;

/** The open Deployment Page and the canvas nodes it lights, for the canvas to bring into view; null when no page is open. */
export function useDeploymentFocus() {
  const lighting = use(LightingContext);
  return lighting ? { key: lighting.deploymentId, nodeIds: [...lighting.lit.keys()] } : null;
}

/** A canvas node under an open Deployment Page: its outcome when the attempt changed it, `null` to dim it; `undefined` when no page is open. */
export function useNodeLighting(nodeId: string) {
  const lighting = use(LightingContext);
  if (!lighting) return undefined;
  return lighting.lit.get(nodeId) ?? null;
}

/**
 * The attempt whose Deployment Page is open, for the scene that stays mounted under it: the canvas lighting, and keeping
 * "open deployments I start" in step with how the user watches their own running attempts.
 */
export function useOpenDeployment(): Lighting {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const deploymentId = useCanvasInspectorSelection().deploymentId;
  const store = useCachedStoreView(organizationSlug, deploymentId ? deploymentQuery(deploymentId) : null);
  if (!store?.ok) return null;
  const { id, status, nodes } = store.value;
  return {
    deploymentId: id,
    lit: new Map(nodes.map((node) => [node.id, { outcome: nodeLight(node.outcome, status), label: nodeOutcomeLabel(node.outcome, status) }])),
  };
}
