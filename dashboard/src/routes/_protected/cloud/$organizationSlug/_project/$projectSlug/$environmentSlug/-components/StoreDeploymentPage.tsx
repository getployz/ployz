import { Suspense, useState, type ReactNode } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import type { BuildView, DeploymentSummary, DeploymentView, NodeOutcome } from "@ployz/sdk";
import { DatabaseIcon, GitBranchPlusIcon, MoreVerticalIcon } from "lucide-react";
import { toast } from "sonner";
import { ContainerLogs } from "#/components/container-logs";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { LogEmpty } from "#/components/log-scroll";
import {
  AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { RelativeTime, RunningTime } from "#/components/relative-time";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Skeleton } from "#/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import {
  admission, canFixOnBranch, deploymentActions, deploymentByline, deploymentStatusIcons, deploymentStatusLabel, focusedService, missingDeployLogs,
  nodeLight, nodeOutcomeLabel, nodeStatusLabels, type DeploymentAction,
} from "#/modules/config-store/store-deployments";
import { buildLogQuery, deploymentQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { formatDuration } from "#/utils/relative-time";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { DEPLOYMENT_PAGE_ROUTE_TO, type deploymentPageSearchSchema } from "./deployment-page";
import { ENVIRONMENT_NEW_BRANCH_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";

/** Past this many Services the tabs become a dropdown. */
const MAX_TABS = 6;
const BUILT = new Set<BuildView["status"]>(["built", "reused"]);
type LogStage = NonNullable<typeof deploymentPageSearchSchema.Type["logs"]>;

/**
 * One Config Store Deployment, the CLI's or this dashboard's, as a panel over the canvas (which lights its nodes). The header
 * holds its number, status, duration and the action its status allows, over who started it, what it ships and when; why
 * it failed sits under it with the fix. Then a tab per Service, its changed Volumes beside them, and that Service's logs.
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
  const focused = focusedService(deployment, search.service);
  const reason = deployment.outcome?.reason;
  const needsSource = deployment.outcome?.type === "not_executed" ? deployment.outcome.needs_upload : [];
  const fixing = focused && canFixOnBranch(deployment, focused, noServers) ? focused.name : null;
  const { at, started, ended } = admission(deployment);
  const byline = deploymentByline(deployment, focused?.name);
  const pick = (next: { service: string; logs?: LogStage }) =>
    void navigate({ to: ".", search: (previous) => ({ ...previous, logs: undefined, ...next }), replace: true });
  const pickService = (id: string | null) => {
    const node = services.find((candidate) => candidate.id === id);
    if (node) pick({ service: node.id });
  };
  const logs = focused
    ? <ServiceLogs key={focused.id} deployment={deployment} node={focused} picked={search.logs} onPick={(stage) => pick({ service: focused.id, logs: stage })} />
    : null;
  const volumeMarks = volumes.map((node) => (
    <span key={node.id} className="flex shrink-0 items-center gap-1.5 text-muted-foreground [&_svg]:size-4">
      <DatabaseIcon aria-hidden />{node.name}
      {node.outcome === "removed" ? <Removed node={node} /> : <OutcomeIcon node={node} deployment={deployment} />}
    </span>
  ));

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params} actions={<StoreDeploymentActions deployment={deployment} noServers={noServers} />}>
        <div className="flex min-w-0 flex-col">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate font-medium">
              {deployment.message ? `#${deployment.number} · ${deployment.message}` : `Deployment #${deployment.number}`}
            </span>
            <span className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground [&_svg]:size-3.5">
              <DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />
              {/* A phone keeps the icon; the word stays for screen readers. */}
              <span className="max-sm:sr-only">{deploymentStatusLabel(deployment)}</span>
            </span>
            {/* One that ended as it started has no duration to show. */}
            {started && ended?.getTime() !== started.getTime() ? (
              <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">
                {ended ? formatDuration((ended.getTime() - started.getTime()) / 1000) : <RunningTime from={started} />}
              </span>
            ) : null}
          </span>
          <p className="truncate text-xs text-muted-foreground">
            {byline}{byline && at ? " · " : null}{at ? <RelativeTime date={at} /> : null}
          </p>
        </div>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
        {reason || fixing ? (
          <div className="flex flex-wrap items-center gap-2">
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

        {!focused ? <>
          {volumes.length ? <NodeRow>{volumeMarks}</NodeRow> : null}
          <p className="text-muted-foreground">No service in this deployment.</p>
        </> : services.length > MAX_TABS ? (
          <div className="flex min-h-64 flex-1 flex-col gap-2">
            <NodeRow>
              <Select value={focused.id} onValueChange={pickService}>
                <SelectTrigger aria-label="Service" className="w-64"><SelectValue>{focused.name}</SelectValue></SelectTrigger>
                <SelectContent>
                  {services.map((node) => (
                    <SelectItem key={node.id} value={node.id}>{node.name} · {nodeOutcomeLabel(node.outcome, deployment)}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {volumeMarks}
            </NodeRow>
            {logs}
          </div>
        ) : (
          <Tabs value={focused.id} onValueChange={pickService} className="min-h-64 flex-1">
            <NodeRow>
              <TabsList variant="line" aria-label="Services">
                {services.map((node) => (
                  <TabsTrigger key={node.id} value={node.id}>
                    {node.outcome === "removed" ? <>{node.name}<Removed node={node} /></> : <><OutcomeIcon node={node} deployment={deployment} />{node.name}</>}
                  </TabsTrigger>
                ))}
              </TabsList>
              {volumeMarks}
            </NodeRow>
            {/* Only the picked Service's panel mounts, so one is all there is to render. */}
            <TabsContent value={focused.id} className="flex min-h-0 flex-col">{logs}</TabsContent>
          </Tabs>
        )}
      </div>
    </div>
  );
}

/** The row of the Deployment's Services and changed Volumes, scrolling sideways when it overflows. */
function NodeRow({ children }: { children: ReactNode }) {
  return <div className="flex shrink-0 items-center gap-3 overflow-x-auto">{children}</div>;
}

/** A node's outcome as its icon, named on hover and for screen readers. */
function OutcomeIcon({ node, deployment }: { node: NodeOutcome; deployment: Pick<DeploymentSummary, "status" | "in_flight"> }) {
  const label = nodeOutcomeLabel(node.outcome, deployment);
  return (
    <span title={label} className="inline-flex [&_svg]:size-4">
      <DeploymentStatusIcon status={nodeLight(node.outcome, deployment)} /><span className="sr-only">{label}</span>
    </span>
  );
}

/** A removed node, said in words; a Volume's reads red, its data gone with it. */
function Removed({ node }: { node: NodeOutcome }) {
  return node.type === "volume"
    ? <span className="text-destructive">Deleted</span>
    : <span className="text-muted-foreground">{nodeStatusLabels.removed}</span>;
}

/**
 * One Service's logs in the Deployment. Where it builds, Build | Deploy follows the running stage until one is picked;
 * its deploy logs say so when the Deployment never reached it.
 */
function ServiceLogs({ deployment, node, picked, onPick }: {
  deployment: DeploymentView; node: NodeOutcome; picked: LogStage | undefined; onPick: (logs: LogStage) => void;
}) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const build = deployment.builds.find((candidate) => candidate.service === node.name);
  const missing = missingDeployLogs(deployment, node);
  const deploy = missing
    ? <LogEmpty title={missing} />
    : <ContainerLogs selection={{ organizationSlug, deploymentId: deployment.id, serviceId: node.id }} />;
  if (!build) return deploy;
  // A build still going or failed is where to look; else how it deployed.
  const stage = picked ?? (BUILT.has(build.status) ? "deploy" : "build");
  return (
    <Tabs value={stage} onValueChange={(value) => { if (value === "build" || value === "deploy") onPick(value); }} className="min-h-0 flex-1">
      <TabsList aria-label="Logs">
        <TabsTrigger value="build">Build</TabsTrigger>
        <TabsTrigger value="deploy">Deploy</TabsTrigger>
      </TabsList>
      <TabsContent value="build" className="flex min-h-0 flex-col gap-2">
        {build.message ? <p className="break-words text-destructive">{build.message}</p> : null}
        <Suspense fallback={<Skeleton className="h-24 w-full" />}>
          <StoreBuildLog deploymentId={deployment.id} service={build.service} />
        </Suspense>
      </TabsContent>
      <TabsContent value="deploy" className="flex min-h-0 flex-col">{deploy}</TabsContent>
    </Tabs>
  );
}

/** A Service's build output in the Deployment, as its Builder reported it; the change stream brings each new chunk. */
function StoreBuildLog({ deploymentId, service }: { deploymentId: string; service: string }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const result = useStoreView(organizationSlug, buildLogQuery(deploymentId, service));
  if (!result.ok) return <p className="text-muted-foreground">{result.refusal.message}</p>;
  return (
    <pre aria-label={`${service} build output`} tabIndex={0} className="min-h-0 flex-1 overflow-auto font-mono text-xs leading-6 break-words whitespace-pre-wrap">
      {result.value.log || "No output yet."}
    </pre>
  );
}

/**
 * The action a Deployment's status allows, one at a time so the header fits a phone: a queued one's Cancel waits in ⋮.
 * Whoever admitted it, CLI or dashboard: they share one queue per Environment.
 */
function StoreDeploymentActions({ deployment, noServers }: { deployment: DeploymentView; noServers: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const writer = useStoreWriter(params.organizationSlug);
  const [cancelOpen, setCancelOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const { primary, cancelInMenu } = deploymentActions(deployment.status, noServers);

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

  // Retry ships what the Deployment froze; Deploy now starts a queued one nothing is running yet.
  const buttons = {
    add_server: <Button size="sm" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/servers" params={params} />}>Add a server</Button>,
    retry: <Button size="sm" variant="outline" disabled={retrying} onClick={() => void retry()}>Retry</Button>,
    start: (
      <Button size="sm" variant="outline" onClick={() => {
        writer.commit({ command: "start", deployment: deployment.id });
        toast.success(`Deployment #${deployment.number} starts now`);
      }}>Deploy now</Button>
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
