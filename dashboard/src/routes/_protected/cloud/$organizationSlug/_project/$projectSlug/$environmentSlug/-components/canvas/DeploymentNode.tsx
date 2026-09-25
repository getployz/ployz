import type { ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { Handle, Position } from "@xyflow/react";
import { CheckIcon, CircleIcon, HardDriveIcon, XIcon } from "lucide-react";
import { formatDuration } from "#/components/deployment-logs";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { Spinner } from "#/components/ui/spinner";
import { cn } from "#/lib/utils";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { nodeOutcomeLabels, type Stage } from "#/modules/deployments/deployment-view";
import { canvasNodeTransition } from "./constants";
import { getServiceIcon, getServiceSubtitle } from "./service-node-helpers";
import type { CanvasDeploymentNodeData } from "./types";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";
import { useDeploymentMode } from "../deployment-mode";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { preloadDeploymentLogs } from "#/modules/deployments/deployment-log.collection";

const stageIcons = {
  done: <CheckIcon className="size-3" aria-label="done" />,
  failed: <XIcon className="size-3" aria-label="failed" />,
  running: <Spinner className="size-3" aria-label="running" />,
  queued: <CircleIcon className="size-3" aria-label="queued" />,
};

/** One stage of Build → Deploy: its state and, once finished, its duration. "—" when it has nothing to do. */
function StageLabel({ name, stage }: { name: string; stage: Stage }) {
  if (stage.state === "none" || stage.state === "skipped") return <span data-stage={name}>{name} —</span>;
  return (
    <span data-stage={name} className={cn("inline-flex items-center gap-1", stage.state === "done" && "text-success", stage.state === "failed" && "text-destructive", stage.state === "running" && "text-info")}>
      {stageIcons[stage.state]}{name}{stage.durationMs === undefined ? null : ` ${formatDuration(stage.durationMs)}`}
    </span>
  );
}

/**
 * A node's card in Deployment Mode: read-only, badged with its Node Outcome, with Build → Deploy and a two-line tail.
 * Unchanged and Not attempted nodes show only their name and outcome, dimmed.
 */
export function DeploymentNodeCard({ data, className }: { data: CanvasDeploymentNodeData; className?: string }) {
  const { node, view } = data;
  const source = node.nodeType === "service" ? node.config.source : null;
  const subtitle = source ? getServiceSubtitle({ source }) : "Named volume";
  const dimmed = view.outcome === "unchanged" || view.outcome === "not_attempted";
  const badge = outcomeBadges[view.outcome];
  return (
    <Card size="node" state={badge === "destructive" || badge === "info" ? badge : undefined}
      data-canvas-node={node.nodeId} data-dimmed={dimmed} className={cn("justify-between", dimmed && "opacity-40", className)}>
      <CardHeader>
        <div className="flex items-start gap-3">
          <Avatar>
            <AvatarFallback>{source ? getServiceIcon({ source }) : <HardDriveIcon />}</AvatarFallback>
          </Avatar>
          <div className="min-w-0 flex-1 overflow-hidden">
            <CardTitle className="truncate">{data.name}</CardTitle>
            {subtitle && !dimmed ? <CardDescription className="truncate">{subtitle}</CardDescription> : null}
          </div>
          <Badge variant={badge}>{nodeOutcomeLabels[view.outcome]}</Badge>
        </div>
      </CardHeader>
      {dimmed ? null : (
        <CardContent className="flex flex-col gap-2">
          <div className="flex items-center gap-1.5 whitespace-nowrap text-muted-foreground">
            <StageLabel name="Build" stage={view.build} />
            <span aria-hidden className="opacity-50">→</span>
            <StageLabel name="Deploy" stage={view.deploy} />
          </div>
          {view.tail.length ? (
            <div data-tail className="font-mono text-xs text-muted-foreground">
              {view.tail.map((line, index) => (
                <div key={index} className={cn("truncate", view.failure ? "text-destructive" : index === view.tail.length - 1 && "text-foreground")}>{line}</div>
              ))}
            </div>
          ) : null}
        </CardContent>
      )}
    </Card>
  );
}

/** Service nodes open the read-only Deployment Mode panel; the retained `deployment` param keeps the mode. */
export function DeploymentNodeLink({ data, className, children }: { data: CanvasDeploymentNodeData; className?: string; children: ReactNode }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const scope = useCollectionScope();
  const attempt = useDeploymentMode();
  const transition = canvasNodeTransition(data.node.nodeId);
  if (data.node.nodeType !== "service") return <div {...transition} className={className}>{children}</div>;
  const warm = () => { if (attempt) preloadDeploymentLogs(params.organizationSlug, attempt.deployment.id, scope); };
  return <Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId: data.node.nodeId }} search={(previous) => ({ ...previous, tab: undefined })}
    onPointerEnter={warm} onFocus={warm} data-canvas-node={data.node.nodeId} {...transition} draggable={false} className={cn("block", className)}>{children}</Link>;
}

export function DeploymentNode({ data }: { data: CanvasDeploymentNodeData }) {
  return (
    <DeploymentNodeLink data={data} className="min-h-36 w-72">
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      <DeploymentNodeCard data={data} className="min-h-36" />
    </DeploymentNodeLink>
  );
}
