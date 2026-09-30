import { Suspense, useState, useSyncExternalStore } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import type { BuildView, DeploymentView } from "@ployz/sdk";
import { DatabaseIcon, GitBranchPlusIcon, MoreVerticalIcon } from "lucide-react";
import { toast } from "sonner";
import { ContainerLogs } from "#/components/container-logs";
import { deploymentBadges } from "#/components/deployment-outcome-badges";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import {
  AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { RelativeTime } from "#/components/relative-time";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Skeleton } from "#/components/ui/skeleton";
import { Tabs, TabsList, TabsTrigger } from "#/components/ui/tabs";
import {
  admission, deploymentActions, deploymentStatusIcons, deploymentStatusLabels, failureReason, formatDuration, nodeApplied, nodeLight,
  nodeLightLabels, notExecuted, uploadLabel, type NodeLight,
} from "#/modules/config-store/store-deployments";
import { buildLogQuery, deploymentQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { DEPLOYMENT_PAGE_ROUTE_TO, type deploymentPageSearchSchema } from "./deployment-page";
import { ENVIRONMENT_NEW_BRANCH_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";

const BUILT = new Set<BuildView["status"]>(["built", "reused"]);

/**
 * One Config Store Deployment, the CLI's or this dashboard's, as a panel over the canvas (which lights its nodes). The header
 * holds its number, status, duration and the action its status allows, over who started it, the commit and when; why it
 * failed sits under it with the fix. Then a tab per Service, its changed Volumes beside them, and the picked Service's
 * Build | Deploy logs filling the rest.
 */
export function StoreDeploymentPage({ deploymentId, search }: { deploymentId: string; search: typeof deploymentPageSearchSchema.Type }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const result = useStoreView(params.organizationSlug, deploymentQuery(deploymentId));
  // With no Server, nothing can run it: the way on is adding one.
  const { noServers } = useRuntimeLens(params.organizationSlug);
  if (!result.ok) {
    return <>
      <CanvasInspectorHeader params={params}><span className="font-medium">Deployment</span></CanvasInspectorHeader>
      <Empty variant="placeholder"><EmptyDescription>This environment has no such deployment.</EmptyDescription></Empty>
    </>;
  }
  const deployment = result.value;
  const services = deployment.nodes.filter((node) => node.type === "service");
  const volumes = deployment.nodes.filter((node) => node.type === "volume");
  const focused = services.find((node) => node.id === search.service)
    ?? services.find((node) => !nodeApplied(node.outcome)) ?? services[0];
  const build = deployment.builds.find((candidate) => candidate.service === focused?.name);
  // A build still going or failed is where to look; else how it deployed. A prebuilt image only deploys.
  const tab = build ? search.logs ?? (BUILT.has(build.status) ? "deploy" : "build") : "deploy";
  const skipped = notExecuted(deployment.outcome);
  const failure = skipped ? null : failureReason(deployment.outcome);
  // A failed Deployment's focused Service it didn't apply can be fixed on a Branch, with the change that failed.
  // With no Server, the failure is having none: Add a server is the one way on.
  const fixing = deployment.status === "failed" && !noServers && focused && !nodeApplied(focused.outcome) ? focused.name : null;
  const light = deploymentStatusIcons[deployment.status];
  const { by, at, started, ended } = admission(deployment);
  // The focused Service's commit when it builds from Git, else the first Git Service's; an upload names its own.
  const commit = deployment.upload ? null : (build?.commit ?? deployment.builds.find((candidate) => candidate.commit)?.commit);
  const pick = (next: { service?: string; logs?: "build" | "deploy" }) => void navigate({ to: ".", search: (previous) => ({ ...previous, ...next }), replace: true });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params} actions={<StoreDeploymentActions deployment={deployment} noServers={noServers} />}>
        <div className="flex min-w-0 flex-col">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate font-medium">Deployment #{deployment.number}</span>
            <Badge variant={deploymentBadges[light]}><DeploymentStatusIcon status={light} />{deploymentStatusLabels[deployment.status]}</Badge>
            {started ? <Elapsed from={started} to={ended} /> : null}
          </span>
          <p className="truncate text-xs text-muted-foreground [&>*+*]:before:px-1.5 [&>*+*]:before:content-['·']">
            {deployment.message ? <span>{deployment.message}</span> : null}
            {deployment.upload ? <span>{uploadLabel(deployment.upload)}</span> : by ? <span>by {by}</span> : null}
            {commit ? <span className="font-mono">{commit.slice(0, 7)}</span> : null}
            {at ? <RelativeTime date={at} /> : null}
          </p>
        </div>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-4 pt-2 pb-4">
        {skipped ? (
          <div className="flex flex-wrap items-center gap-2">
            <p className="break-words text-destructive">{skipped.reason}</p>
            {/* A Service with nothing to run: the way on is giving it an image, in its drawer. */}
            {skipped.needsSource.flatMap((name) => services.filter((node) => node.name === name)).map((node) => (
              <Button key={node.id} size="sm" variant="outline" nativeButton={false}
                render={<Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId: node.id }} />}>
                Add an image to {node.name}
              </Button>
            ))}
          </div>
        ) : failure || fixing ? (
          <div className="flex flex-wrap items-center gap-2">
            {failure ? <p className="min-w-0 break-words text-destructive">{failure}</p> : null}
            {fixing ? (
              <Button size="sm" variant="outline" nativeButton={false} render={<Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO}
                params={params} search={{ focus: fixing, fix: deployment.id }} />}>
                <GitBranchPlusIcon data-icon="inline-start" />Fix it on a branch
              </Button>
            ) : null}
          </div>
        ) : null}

        <div className="flex shrink-0 items-center gap-3">
          <div className="flex min-w-0 items-center gap-3 overflow-x-auto">
            {services.length ? (
              <Tabs value={focused?.id ?? null} onValueChange={(value) => {
                const node = services.find(({ id }) => id === value);
                if (node) pick({ service: node.id, logs: undefined });
              }}>
                <TabsList variant="line" aria-label="Services">
                  {services.map((node) => (
                    <TabsTrigger key={node.id} value={node.id} className="flex-none">
                      <Light light={nodeLight(node.outcome, deployment.status)} label={node.outcome === "removed" ? "Removed" : undefined} />
                      {node.name}
                    </TabsTrigger>
                  ))}
                </TabsList>
              </Tabs>
            ) : null}
            {volumes.map((node) => (
              <span key={node.id} className="flex shrink-0 items-center gap-1.5 text-muted-foreground [&_svg]:size-4">
                <DatabaseIcon aria-hidden />{node.name}
                {/* A removed Volume's data is gone: say so. */}
                {node.outcome === "removed" ? <span className="text-destructive">Deleted</span> : <Light light={nodeLight(node.outcome, deployment.status)} />}
              </span>
            ))}
          </div>
          {build ? (
            <Tabs value={tab} onValueChange={(logs) => { if (logs === "build" || logs === "deploy") pick({ service: focused?.id, logs }); }} className="ml-auto">
              <TabsList aria-label="Logs">
                <TabsTrigger value="build">Build</TabsTrigger>
                <TabsTrigger value="deploy">Deploy</TabsTrigger>
              </TabsList>
            </Tabs>
          ) : null}
        </div>

        <div className="flex min-h-64 flex-1 flex-col gap-2">
          {!focused ? <p className="text-muted-foreground">No service in this deployment.</p>
            : tab === "build" && build ? <>
              {build.message ? <p className="break-words text-destructive">{build.message}</p> : null}
              <Suspense fallback={<Skeleton className="h-24 w-full" />}>
                <StoreBuildLog deploymentId={deployment.id} service={build.service} />
              </Suspense>
            </>
            : <ContainerLogs selection={{ organizationSlug: params.organizationSlug, deploymentId: deployment.id, serviceId: focused.id }} />}
        </div>
      </div>
    </div>
  );
}

/** A node's outcome as its icon, named for screen readers and on hover. */
function Light({ light, label = nodeLightLabels[light] }: { light: NodeLight; label?: string }) {
  return <span title={label} className="inline-flex [&_svg]:size-4"><DeploymentStatusIcon status={light} /><span className="sr-only">{label}</span></span>;
}

const everySecond = (tick: () => void) => {
  const timer = setInterval(tick, 1000);
  return () => clearInterval(timer);
};
const never = () => () => {};

/**
 * How long it ran, or has run so far, ticking while it runs; nothing for one that ended as it started. It reads the
 * clock, so it renders only in the browser.
 */
function Elapsed({ from, to }: { from: Date; to: Date | null }) {
  const now = useSyncExternalStore(to ? never : everySecond, () => Math.floor(Date.now() / 1000), () => null);
  const seconds = ((to?.getTime() ?? (now ?? 0) * 1000) - from.getTime()) / 1000;
  if (now === null || (to && seconds < 1)) return null;
  return <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">{formatDuration(seconds)}</span>;
}

/** A Service's build output in the Deployment, as its Builder reported it; the change stream brings each new chunk. */
function StoreBuildLog({ deploymentId, service }: { deploymentId: string; service: string }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const result = useStoreView(organizationSlug, buildLogQuery(deploymentId, service));
  if (!result.ok) return <p className="text-muted-foreground">{result.refusal.message}</p>;
  return (
    <pre aria-label="Build logs" tabIndex={0} className="min-h-0 flex-1 overflow-auto font-mono text-xs leading-6 break-words whitespace-pre-wrap">
      {result.value.log || "No output yet."}
    </pre>
  );
}

/**
 * Retry an ended Deployment that didn't apply (it ships what it froze), Deploy now a queued one nothing is running
 * yet, Cancel one before it ends. Whoever admitted it, CLI or dashboard: they share one queue per Environment. One
 * action shows, so the header fits a phone; a queued one's Cancel waits in ⋮.
 */
function StoreDeploymentActions({ deployment, noServers }: { deployment: DeploymentView; noServers: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const writer = useStoreWriter(params.organizationSlug);
  const [cancelOpen, setCancelOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const actions = deploymentActions(deployment.status);
  const primary = noServers && (actions.retry || actions.start) ? "add-server"
    : actions.retry ? "retry" : actions.start ? "start" : actions.cancel ? "cancel" : null;

  async function retry() {
    const id = crypto.randomUUID();
    setRetrying(true);
    try {
      await writer.commit({
        command: "admit", admit: "retry", id, deployment: deployment.id }).isPersisted.promise;
      void navigate({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId: id } });
    } catch {
      // The writer toasted the Store's reason.
    } finally {
      setRetrying(false);
    }
  }

  return (
    <>
      {primary === "add-server" ? (
        <Button size="sm" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/servers" params={params} />}>Add a server</Button>
      ) : null}
      {primary === "retry" ? <Button size="sm" variant="outline" disabled={retrying} onClick={() => void retry()}>Retry</Button> : null}
      {primary === "start" ? (
        <Button size="sm" variant="outline" onClick={() => {
          writer.commit({ command: "start", deployment: deployment.id });
          toast.success(`Deployment #${deployment.number} starts now`);
        }}>Deploy now</Button>
      ) : null}
      {primary === "cancel" ? <Button size="sm" variant="outline" onClick={() => setCancelOpen(true)}>Cancel</Button> : null}
      {actions.cancel && primary !== "cancel" ? (
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
              {deployment.status === "queued"
                ? "It leaves the queue and never runs."
                : "It stops at its next step. What it already changed stays until the next Deploy."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep deploying</AlertDialogCancel>
            <Button variant="destructive" onClick={() => {
              setCancelOpen(false);
              writer.commit({ command: "cancel", deployment: deployment.id });
            }}>Cancel deployment</Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
