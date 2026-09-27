import { useState } from "react";
import { Link, useLoaderData, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useSuspenseQuery } from "@tanstack/react-query";
import { parseServiceConfig } from "@ployz/sdk/config";
import { CancelDeploymentDialog } from "#/components/cancel-deployment-dialog";
import { formatDuration, ServiceBuildLogs, ServiceDeployLogs } from "#/components/deployment-logs";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemContent, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { useDeployQueuedNow, useRetryDeployment } from "#/modules/deployments/deployment-commands";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { deploymentAttemptQueryOptions } from "#/modules/deployments/deployment-history.queries";
import { useDeploymentAttempt, useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import {
  deploymentLighting, deploymentLogTab, deploymentStatusLabel, hasBuildLogs, nodeOutcomeLabels, shortDeploymentId,
  type DeploymentNodeView, type TargetNode,
} from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { DEPLOYMENT_PAGE_ROUTE_ID } from "./deployment-page";
import { useEnvironmentNavigationNodes } from "./environment-node-navigation";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

/** Past this many changed services the chips become a dropdown. */
const MAX_CHIPS = 6;
const inFlight = new Set<DeploymentNodeView["outcome"]>(["failed", "building", "deploying"]);

/**
 * One Cloud Deployment Attempt as a panel over the live canvas: its header and actions, a chip per changed service,
 * and that service's Build | Deploy logs. The canvas underneath lights up what it changed (useOpenDeployment).
 */
export function DeploymentPage({ deploymentId }: { deploymentId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const search = useSearch({ from: DEPLOYMENT_PAGE_ROUTE_ID });
  const navigate = useNavigate();
  // The loader prefetched this read; it names who started the attempt and the configs it deployed.
  const { data: read } = useSuspenseQuery(deploymentAttemptQueryOptions(params.organizationSlug, deploymentId));
  const { attempt } = useDeploymentAttempt(params.organizationSlug, environmentId, deploymentId, { buildLog: true });
  // The Environment Nodes the canvas draws now.
  const onCanvas = new Set(useEnvironmentNavigationNodes(params).nodes.map((node) => node.id));
  if (!attempt) {
    return <>
      <CanvasInspectorHeader params={params}><span className="font-medium">Deployment</span></CanvasInspectorHeader>
      <Empty variant="placeholder"><EmptyDescription>This environment has no such deployment.</EmptyDescription></Empty>
    </>;
  }
  const { deployment, view } = attempt;
  const { offCanvas } = deploymentLighting(attempt, onCanvas);
  const services = attempt.nodes.flatMap((node) => {
    const nodeView = view.nodes.find((candidate) => candidate.nodeId === node.nodeId);
    return node.nodeType === "service" && nodeView && nodeView.outcome !== "unchanged" ? [{ node, view: nodeView }] : [];
  });
  const focused = services.find(({ node }) => node.nodeId === search.service)
    ?? services.find(({ view }) => inFlight.has(view.outcome)) ?? services[0];
  const tab = focused ? deploymentLogTab(focused.view, search.logs) : "deploy";
  const config = focused ? read?.serviceConfigs.find((row) => row.nodeId === focused.node.nodeId) : undefined;
  const source = config ? parseServiceConfig(config.config).source : null;
  const branch = source?.type === "git" ? (source.branch.type === "connected" ? source.branch.name : source.branch.previousName) : null;
  const commit = focused ? deployment.sourcePins[focused.node.nodeId]?.commitSha.slice(0, 7) : undefined;
  const pageSearch = (service: string) => ({ service, logs: undefined });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="flex min-w-0 items-center gap-2 font-medium [&_svg]:size-4">
          <DeploymentStatusIcon status={view.status} />
          <span className="truncate">Deployment <span className="font-mono">{shortDeploymentId(deployment.id)}</span></span>
        </span>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
        <header className="flex flex-col gap-1">
          <p className="text-xs text-muted-foreground">
            <span className="font-mono">{shortDeploymentId(deployment.id)}</span> · {triggerLabel(deployment, read?.actorName ?? null)}
          </p>
          <div className="flex flex-wrap items-start justify-between gap-2">
            <h2 className="min-w-0 text-base font-medium break-words">{deployment.message ?? "Deployment"}</h2>
            <DeploymentActions deployment={deployment} />
          </div>
          <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-muted-foreground [&_svg]:size-3.5">
            <span className="inline-flex items-center gap-1 text-foreground"><DeploymentStatusIcon status={view.status} />{deploymentStatusLabel(view)}</span>
            <Duration deployment={deployment} />
            <RelativeTime date={deployment.createdAt} />
            {branch ? <span className="font-mono">{branch}{commit ? ` @ ${commit}` : null}</span> : null}
          </p>
        </header>

        {services.length > MAX_CHIPS ? (
          <Select value={focused?.node.nodeId ?? null}
            onValueChange={(service) => { if (service) void navigate({ to: ".", search: pageSearch(service), replace: true }); }}>
            <SelectTrigger aria-label="Service" className="w-full sm:w-64"><SelectValue /></SelectTrigger>
            <SelectContent>
              {services.map(({ node, view: nodeView }) => (
                <SelectItem key={node.nodeId} value={node.nodeId}>{node.name} · {nodeOutcomeLabels[nodeView.outcome]}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : services.length > 1 ? (
          <nav aria-label="Services" className="flex flex-wrap gap-1.5">
            {services.map(({ node, view: nodeView }) => {
              const current = node.nodeId === focused?.node.nodeId;
              return (
                <Link key={node.nodeId} to="." search={pageSearch(node.nodeId)} replace aria-current={current ? "true" : undefined}
                  className={buttonVariants({ size: "sm", variant: current ? "secondary" : "outline" })}>
                  {node.name}
                  <Badge variant={outcomeBadges[nodeView.outcome]}>{nodeOutcomeLabels[nodeView.outcome]}</Badge>
                </Link>
              );
            })}
          </nav>
        ) : null}

        {focused ? (
          <Tabs value={tab} onValueChange={(value) => {
            if (value === "build" || value === "deploy") void navigate({ to: ".", search: (previous) => ({ ...previous, service: focused.node.nodeId, logs: value }), replace: true });
          }} className="flex min-h-80 flex-1 flex-col">
            <div className="flex items-center gap-2">
              {services.length === 1 ? <span className="font-medium">{focused.node.name}</span> : null}
              <TabsList variant="line">
                <TabsTrigger value="build" disabled={!hasBuildLogs(focused.view)}
                  title={hasBuildLogs(focused.view) ? undefined : "Prebuilt image, nothing built"}>Build</TabsTrigger>
                <TabsTrigger value="deploy">Deploy</TabsTrigger>
              </TabsList>
              {services.length === 1 ? <Badge variant={outcomeBadges[focused.view.outcome]} className="ml-auto">{nodeOutcomeLabels[focused.view.outcome]}</Badge> : null}
            </div>
            {focused.view.failure ? <p className="mt-2 break-words text-destructive">{focused.view.failure.message}</p> : null}
            <TabsContent value="build" className="mt-3 flex min-h-0 flex-1 flex-col">
              <ServiceBuildLogs organizationSlug={params.organizationSlug} deploymentId={deployment.id} image={focused.node.name} />
            </TabsContent>
            <TabsContent value="deploy" className="mt-3 flex min-h-0 flex-1 flex-col">
              <ServiceDeployLogs organizationSlug={params.organizationSlug} deploymentId={deployment.id} serviceId={focused.node.nodeId} />
            </TabsContent>
          </Tabs>
        ) : (
          <p className="text-muted-foreground">No service changed in this deployment.</p>
        )}

        {offCanvas.length ? <OffCanvasNodes nodes={offCanvas} /> : null}
      </div>
    </div>
  );
}

/** Nodes the attempt changed that the canvas no longer draws: it removed them, or they were deleted since. */
function OffCanvasNodes({ nodes }: { nodes: { node: TargetNode; view: DeploymentNodeView }[] }) {
  return (
    <section className="flex flex-col gap-2">
      <h3 className="font-medium">Not on the canvas</h3>
      <ItemGroup className="gap-2">
        {nodes.map(({ node, view }) => (
          <Item key={node.nodeId} variant="outline" size="sm">
            <ItemContent className="min-w-0"><ItemTitle><span className="truncate">{node.name}</span></ItemTitle></ItemContent>
            <Badge variant={outcomeBadges[view.outcome]}>{nodeOutcomeLabels[view.outcome]}</Badge>
          </Item>
        ))}
      </ItemGroup>
    </section>
  );
}

function triggerLabel(deployment: EnvironmentDeploymentSummary, actorName: string | null) {
  switch (deployment.triggerOrigin.origin) {
    case "manual": return actorName ? `Manual · ${actorName}` : "Manual";
    case "github": return "Git push";
    case "first_connect": return "First server connected";
  }
}

/** How long the attempt ran, or has run so far. */
function Duration({ deployment }: { deployment: EnvironmentDeploymentSummary }) {
  if (!deployment.startedAt) return null;
  const end = deployment.finishedAt?.getTime() ?? Date.now();
  // A running attempt's duration reads the clock, which moves between SSR and hydration.
  return <span className="tabular-nums" suppressHydrationWarning>{formatDuration(end - deployment.startedAt.getTime())}</span>;
}

/**
 * Retry on a failed attempt, Deploy now on one queued for the next trigger, Cancel on a queued or running one; all keep their
 * existing semantics. Nothing redeploys a finished attempt yet, so Redeploy shows greyed out with Soon.
 */
function DeploymentActions({ deployment }: { deployment: EnvironmentDeploymentSummary }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const attempts = useEnvironmentDeployments(organizationSlug, deployment.environmentId);
  const [retry, isRetrying] = useRetryDeployment(deployment);
  const [deployNow, isDispatching] = useDeployQueuedNow(deployment);
  const [cancelOpen, setCancelOpen] = useState(false);
  const active = isActiveDeployment(deployment.status);
  const cancellable = !deployment.cancellationRequestedAt && active;
  // Another queued attempt is building, so this one is the pending attempt and waits for it rather than offering Deploy now.
  const building = attempts.some(({ deployment: other }) => other.status === "queued" && other.inngestRunId !== null && other.id !== deployment.id);
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-2">
      {deployment.canRetry ? <Button size="sm" variant="outline" disabled={isRetrying} onClick={() => void retry()}>Retry</Button> : null}
      {/* Queued with no dispatch requested: it waits for the environment's next trigger. */}
      {deployment.status === "queued" && !deployment.dispatchRequestedAt ? (building
        ? <span className="text-muted-foreground">Waiting for the current build</span>
        : <Button size="sm" variant="outline" disabled={isDispatching} onClick={() => void deployNow()}>Deploy now</Button>) : null}
      {cancellable ? <Button size="sm" variant="outline" onClick={() => setCancelOpen(true)}>Cancel</Button> : null}
      {!active && !deployment.canRetry ? (
        <Button size="sm" variant="outline" disabled>Redeploy<Badge variant="secondary">Soon</Badge></Button>
      ) : null}
      <CancelDeploymentDialog open={cancelOpen} onOpenChange={setCancelOpen} organizationSlug={organizationSlug} deployment={deployment} />
    </div>
  );
}
