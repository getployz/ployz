import { createContext, type ReactNode } from "react";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { ChevronRightIcon, CircleDashedIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { deploymentStatusLabel } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "./deployment-page";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

/** The editor canvas owns the change state, so it portals the apply zone into this slot of the bar. */
export const ApplyZoneSlot = createContext<HTMLElement | null>(null);

/**
 * The floating deploy bar, usable while a panel is open: the running and queued deployments' shortcuts, each opening
 * its Deployment Page. `children` renders after them (the apply zone, #1051).
 */
export function DeployBar({ children }: { children?: ReactNode }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const attempts = useEnvironmentDeployments(params.organizationSlug, environmentId);
  // The oldest queued or running attempt holds, or is next for, the Environment execution slot; the rest wait behind it.
  const active = attempts.filter(({ deployment }) => isActiveDeployment(deployment.status));
  const running = active.at(-1);
  const queued = active.length > 1 ? active[0] : undefined;
  const page = (deploymentId: string) => ({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId } }) as const;

  // A queued or running attempt opens directly, unless its page is already open.
  const openRunning = running && running.deployment.id !== viewedId ? (
    <Link {...page(running.deployment.id)} className={buttonVariants({ size: "sm", variant: "secondary" })}>
      <DeploymentStatusIcon status={running.view.status} />
      <span className="tabular-nums">{running.view.status === "deploying" ? `Deploying ${running.view.deployed}/${running.view.changed}` : deploymentStatusLabel(running.view)}</span>
      <ChevronRightIcon />
    </Link>
  ) : null;
  const openQueued = queued && queued.deployment.id !== viewedId ? (
    <Link {...page(queued.deployment.id)} className={buttonVariants({ size: "sm", variant: "ghost" })}>
      <CircleDashedIcon />{active.length > 2 ? `${active.length - 1} queued` : "Queued"}
    </Link>
  ) : null;

  return (
    <div role="group" aria-label="Deploy bar" className="deploy-bar">
      <div className="flex min-w-0 items-center gap-0.5">
        {/* Intent Pink marks staged changes; styles.css shows it only while the bar holds the apply zone. */}
        <span aria-hidden className="deploy-bar-pending mx-1.5 size-2 rounded-full bg-changed" />
        {openRunning}
        {openQueued}
      </div>
      {children}
    </div>
  );
}
