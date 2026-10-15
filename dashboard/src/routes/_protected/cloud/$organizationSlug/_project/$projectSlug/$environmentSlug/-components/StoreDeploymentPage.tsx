import { useState, type ReactNode } from "react";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import type { DeploymentView } from "@ployz/sdk";
import { ArrowLeftIcon, GitBranchPlusIcon, LockIcon, MoreVerticalIcon, TriangleAlertIcon } from "lucide-react";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { Badge } from "#/components/ui/badge";
import { buttonVariants } from "#/components/ui/button-variants";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import {
  AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { RelativeTime, RunningTime } from "#/components/relative-time";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import {
  admission, canFixOnBranch, deploymentActions, deploymentWarningText, deploymentByline, deploymentStatusIcons, deploymentStatusLabel, nodeApplied, outcomeReason, showCancelling,
  type DeploymentAction,
} from "#/modules/config-store/store-deployments";
import { deploymentQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreCancelling } from "#/modules/config-store/store-pending";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { useCopiesWait, useStorageSteps } from "#/modules/volume-placement/volume-placement.hooks";
import { formatDuration } from "#/utils/relative-time";
import { makeRunGraph, selectRunNode, type RunGraph, type RunNode } from "./deployments/run-graph";
import { RunCanvas } from "./deployments/RunCanvas";
import { RunPanel } from "./deployments/RunPanel";
import { deploymentDestination, nodeDestination, VERSIONS_ROUTE_TO, type RunSearch } from "./deployments/run-search";
import { ENVIRONMENT_NEW_BRANCH_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";

/**
 * One Config Store Deployment's Run, the CLI's or this dashboard's: its builds and nodes on a canvas under a header, and
 * the picked node (its `job`) in a panel beside them.
 */
export function StoreDeploymentPage({ deploymentId, search }: { deploymentId: string; search: RunSearch }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const result = useStoreView(organizationSlug, deploymentQuery(deploymentId));
  const writer = useStoreWriter(organizationSlug);
  const cancelling = useStoreCancelling(organizationSlug, deploymentId);
  if (!result.ok) return <Empty variant="placeholder" className="m-4"><EmptyDescription>Deployment not found</EmptyDescription></Empty>;
  const deployment = showCancelling(result.value, cancelling);
  const cancel = () => void writer.commit({ command: "cancel", deployment: deployment.id });
  const graph = makeRunGraph(deployment);
  const selected = selectRunNode(graph, search);
  return (
    // Stacked on a narrow screen, the page scrolls. The canvas keeps its height and the panel gets a screen of its own.
    <div className="flex min-h-0 flex-1 gap-2 p-2 max-lg:flex-col max-lg:overflow-y-auto">
      <div className="relative min-h-80 flex-1 overflow-hidden rounded-xl border max-lg:flex-none">
        <RunCanvas graph={graph} selectedId={selected?.id}
          header={<RunHeader deployment={deployment} graph={graph} selected={selected} search={search} onCancel={cancel} />} />
      </div>
      <RunPanel deployment={deployment} node={selected} />
    </div>
  );
}

/**
 * The Run's header: back to the node it was opened from, its title, status, duration and how many nodes are done, over
 * who started it, the version it ships and when; then why it failed with the fix, and what it shipped despite.
 */
function RunHeader({ deployment, graph, selected, search, onCancel }: {
  deployment: DeploymentView; graph: RunGraph; selected: RunNode | undefined; search: RunSearch; onCancel: () => void;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  // With no Server, nothing can run it: the way on is adding one.
  const { noServers } = useRuntimeLens(params.organizationSlug);
  const storage = useStorageSteps(params.organizationSlug)(deployment.id);
  const warnings = deployment.warnings ?? [];
  const ingressWarnings = warnings.filter((warning) => "ingress" in warning);
  const uncheckedIngress = warnings.filter((warning) => "cause" in warning);
  const variableWarnings = warnings.filter((warning) => !("ingress" in warning) && !("cause" in warning));
  const services = deployment.nodes.filter((node) => node.type === "service");
  // A selected Volume or Config falls back to the Service that failed, so the fix stays one click away.
  const service = selected?.type === "build" ? services.find((node) => node.name === selected.name)
    : selected?.node.type === "service" ? selected.node
    : services.find((node) => node.outcome === "failed") ?? services.find((node) => !nodeApplied(node.outcome));
  const reason = outcomeReason(deployment.outcome);
  const needsSource = deployment.outcome?.type === "not_executed" ? deployment.outcome.needs_upload : [];
  const fixing = service && canFixOnBranch(deployment, service, noServers) ? service.name : null;
  const { at, started, ended } = admission(deployment);
  const byline = deploymentByline(deployment, service?.name);

  return (
    <div className="flex w-xl max-w-full flex-col gap-2 rounded-xl border bg-card p-3 text-card-foreground shadow-sm">
      <div className="flex items-start gap-2">
        <Link {...nodeDestination(params.organizationSlug, params, search.origin)}
          className={buttonVariants({ variant: "ghost", size: "icon-sm" })} aria-label="Back to Architecture" title="Back to Architecture">
          <ArrowLeftIcon />
        </Link>
        <div className="flex min-w-0 flex-1 flex-col">
          <span className="flex min-w-0 items-center gap-2 [&_svg]:size-4">
            <DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />
            <span className="truncate font-medium">
              {deployment.message ? `#${deployment.number} · ${deployment.message}` : `Deployment #${deployment.number}`}
            </span>
          </span>
          <p className="truncate text-xs text-muted-foreground">
            {deploymentStatusLabel(deployment)}
            {/* One that ended as it started has no duration to show. */}
            {started && ended?.getTime() !== started.getTime() ? (
              <>{" · "}<span className="font-mono tabular-nums">
                {ended ? formatDuration((ended.getTime() - started.getTime()) / 1000) : <RunningTime from={started} />}
              </span></>
            ) : null}
            {graph.nodes.length ? ` · ${graph.done} of ${graph.nodes.length}` : null}
          </p>
          <p className="truncate text-xs text-muted-foreground">
            {byline}{byline ? " · " : null}
            <Link to={VERSIONS_ROUTE_TO} params={params} search={{ revision: deployment.saved }}
              className="underline-offset-4 hover:underline">Version #{deployment.saved}</Link>
            {at ? <>{" · "}<RelativeTime date={at} /></> : null}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-3">
          <StoreDeploymentActions deployment={deployment} storage={storage} noServers={noServers} search={search} onCancel={onCancel} />
        </div>
      </div>
      {reason || fixing || needsSource.length ? (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          {reason ? <p className="min-w-0 break-words text-destructive">{reason}</p> : null}
          {/* A Service with nothing to run: the way on is giving it an image, in its drawer. */}
          {needsSource.flatMap((name) => services.filter((node) => node.name === name)).map((node) => (
            <Button key={node.id} size="sm" variant="outline" nativeButton={false}
              render={<Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId: node.id }} />}>
              Add an image to {node.name}
            </Button>
          ))}
          {fixing ? (
            <Button size="sm" variant="outline" nativeButton={false} render={<Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO}
              params={params} search={{ focus: fixing, fix: deployment.id }} />}>
              <GitBranchPlusIcon data-icon="inline-start" />Fix it on a branch
            </Button>
          ) : null}
        </div>
      ) : null}
      {storage ? <p role="status" className="text-sm">{storage}</p> : null}

      {/* What it shipped despite: a secret left out, or a Config file reference left empty. */}
      {variableWarnings.length ? (
        <ul aria-label="Warnings" className="flex flex-wrap gap-1.5">
          {variableWarnings.map((warning) => (
            <li key={deploymentWarningText(warning)}><Badge variant="warning"><LockIcon />{deploymentWarningText(warning)}</Badge></li>
          ))}
        </ul>
      ) : null}
      {ingressWarnings.map((warning) => (
        <p key={warning.server} role="status" className="flex flex-wrap items-center gap-1.5 text-sm text-warning">
          <TriangleAlertIcon className="size-4" />{deploymentWarningText(warning)}.
          <Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug: params.organizationSlug }}
            className="underline underline-offset-4">Organization › General › Domain</Link>
        </p>
      ))}
      {uncheckedIngress.map((warning) => (
        <p key={warning.cause} role="status" className="flex flex-wrap items-center gap-1.5 text-sm text-warning">
          <TriangleAlertIcon className="size-4" />{deploymentWarningText(warning)}.
        </p>
      ))}
    </div>
  );
}

/** Redeploy ships what the Deployment froze, so copy changes staged since then wait for the next Deploy. */
export function RedeployAction({ copiesWait, disabled, onRedeploy }: { copiesWait: boolean; disabled: boolean; onRedeploy: () => void }) {
  return (
    <>
      {copiesWait ? <span className="text-xs text-muted-foreground">Copies wait for Deploy</span> : null}
      <Button size="sm" variant="outline" disabled={disabled} onClick={onRedeploy}>Redeploy</Button>
    </>
  );
}

/**
 * The action a Deployment's status allows, one at a time so the header fits a phone: a queued one's Cancel waits in ⋮.
 * Whoever admitted it, CLI or dashboard: they share one queue per Environment.
 */
function StoreDeploymentActions({ deployment, storage, noServers, search, onCancel }: {
  deployment: DeploymentView; storage: string | null; noServers: boolean; search: RunSearch; onCancel: () => void;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const writer = useStoreWriter(params.organizationSlug);
  const [cancelOpen, setCancelOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const { primary, cancelInMenu } = deploymentActions(deployment.status, noServers);
  const copiesWait = useCopiesWait(params.organizationSlug, useLoaderData({ from: ENVIRONMENT_ROUTE_FROM }).environmentId);

  async function retry() {
    const id = crypto.randomUUID();
    setRetrying(true);
    try {
      await writer.commit({
        command: "admit", admit: "retry", id, deployment: deployment.id }).isPersisted.promise;
      void navigate(deploymentDestination(params, id, { job: search.job, origin: search.origin }));
    } catch {
      // The writer toasted the Store's reason.
    } finally {
      setRetrying(false);
    }
  }

  // Deploy now starts a queued one while nothing else runs.
  const buttons = {
    add_server: <Button size="sm" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/servers" params={params} />}>Add a server</Button>,
    retry: <RedeployAction copiesWait={copiesWait} disabled={retrying} onRedeploy={() => void retry()} />,
    start: (
      <Button size="sm" variant="outline" onClick={() => writer.commit({ command: "start", deployment: deployment.id })}>Deploy now</Button>
    ),
    cancel: <Button size="sm" variant="outline" onClick={() => setCancelOpen(true)}>Cancel</Button>,
  } satisfies Record<DeploymentAction, ReactNode>;

  return (
    <>
      {primary ? buttons[primary] : null}
      {cancelInMenu ? (
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon" aria-label="Deployment actions" title="Deployment actions" />}>
            <MoreVerticalIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-auto">
            <DropdownMenuItem onClick={() => setCancelOpen(true)}>Cancel deployment…</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ) : null}
      <AlertDialog open={cancelOpen} onOpenChange={setCancelOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Cancel deployment?</AlertDialogTitle>
            <AlertDialogDescription>
              {storage ? "The step under way finishes first. The rest waits for your next deploy."
                : deployment.status === "queued"
                ? "It leaves the queue and never runs."
                : "It stops at its next step. What it already changed stays until the next Deploy."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep deploying</AlertDialogCancel>
            <Button variant="destructive" onClick={() => {
              setCancelOpen(false);
              onCancel();
            }}>Cancel deployment</Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
