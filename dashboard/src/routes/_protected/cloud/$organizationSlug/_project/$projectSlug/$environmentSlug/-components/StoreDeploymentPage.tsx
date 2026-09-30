import { Suspense, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import type { BuildView, DeploymentView, NodeOutcome } from "@ployz/sdk";
import { GitBranchPlusIcon } from "lucide-react";
import { ContainerLogs } from "#/components/container-logs";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import {
  AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { RelativeTime } from "#/components/relative-time";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemContent, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Skeleton } from "#/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import {
  admission, deploymentActions, deploymentStatusIcons, deploymentStatusLabels, nodeApplied, nodeLight, nodeStatusLabels, failureReason, previewLines,
  targetsLabel,
  uploadLabel,
} from "#/modules/config-store/store-deployments";
import { buildLogQuery, deploymentQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { DEPLOYMENT_PAGE_ROUTE_TO, type deploymentPageSearchSchema } from "./deployment-page";
import { ENVIRONMENT_NEW_BRANCH_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

const BUILT = new Set<BuildView["status"]>(["built", "reused"]);

/**
 * One Config Store Deployment, the CLI's or this dashboard's, as a panel over the canvas (which lights its nodes): its
 * status and what it ships, Retry / Deploy now / Cancel, why it didn't run, its recorded Deploy Preview, a chip per
 * Service with its Node Outcome, and that Service's Build | Deploy logs.
 */
export function StoreDeploymentPage({ deploymentId, search }: { deploymentId: string; search: typeof deploymentPageSearchSchema.Type }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const result = useStoreView(params.organizationSlug, deploymentQuery(deploymentId));
  if (!result.ok) {
    return <>
      <CanvasInspectorHeader params={params}><span className="font-medium">Deployment</span></CanvasInspectorHeader>
      <Empty variant="placeholder"><EmptyDescription>This environment has no such deployment.</EmptyDescription></Empty>
    </>;
  }
  const deployment = result.value;
  const services = deployment.nodes.filter((node) => node.type === "service");
  const volumes = deployment.nodes.filter((node) => node.type === "volume");
  const buildOf = (node: NodeOutcome | undefined) => deployment.builds.find((build) => build.service === node?.name);
  const focused = services.find((node) => node.id === search.service)
    ?? services.find((node) => !nodeApplied(node.outcome)) ?? services[0];
  const build = buildOf(focused);
  // A build still going or failed is where to look; else how it deployed.
  const tab = search.logs ?? (build && !BUILT.has(build.status) ? "build" : "deploy");
  const failure = failureReason(deployment.outcome);
  const preview = previewLines(deployment.preview);
  const admitted = admission(deployment);
  const pageSearch = (service: string) => ({ service, logs: undefined, returnTo: search.returnTo });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="flex min-w-0 items-center gap-2 font-medium [&_svg]:size-4">
          <DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />
          <span className="truncate">Deployment #{deployment.number}</span>
        </span>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
        <header className="flex flex-col gap-1">
          <p className="text-xs text-muted-foreground">
            Saved revision {deployment.saved}{deployment.upload ? ` · ${uploadLabel(deployment.upload)}` : admitted.by ? ` · by ${admitted.by}` : null}
            {admitted.at ? <> · admitted <RelativeTime date={admitted.at} /></> : null}
            {admitted.started ? <> · started <RelativeTime date={admitted.started} /></> : null}
            {admitted.ended ? <> · ended <RelativeTime date={admitted.ended} /></> : null}
          </p>
          <div className="flex flex-wrap items-start justify-between gap-2">
            <h2 className="min-w-0 text-base font-medium break-words">Deploys {targetsLabel(deployment)}</h2>
            <StoreDeploymentActions deployment={deployment} focused={focused} />
          </div>
          <p className="flex items-center gap-1 [&_svg]:size-3.5">
            <DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />{deploymentStatusLabels[deployment.status]}
          </p>
          {failure ? <p className="break-words text-destructive">{failure}</p> : null}
        </header>

        {preview ? (
          <section aria-label="Deploy Preview" className="flex flex-col gap-1">
            <h3 className="font-medium">Preview</h3>
            <ul className="text-muted-foreground">{preview.map((line) => <li key={line}>{line}</li>)}</ul>
          </section>
        ) : null}

        {services.length > 1 ? (
          <nav aria-label="Services" className="flex flex-wrap gap-1.5">
            {services.map((node) => {
              const current = node.id === focused?.id;
              return (
                <Link key={node.id} to="." search={pageSearch(node.id)} replace aria-current={current ? "true" : undefined}
                  className={buttonVariants({ size: "sm", variant: current ? "secondary" : "outline" })}>
                  {node.name}
                  <Badge variant={outcomeBadges[nodeLight(node.outcome, deployment.status)]}>{nodeStatusLabels[node.outcome]}</Badge>
                </Link>
              );
            })}
          </nav>
        ) : null}

        {focused ? (
          <Tabs value={tab} onValueChange={(value) => {
            if (value === "build" || value === "deploy") void navigate({ to: ".", search: (previous) => ({ ...previous, service: focused.id, logs: value }), replace: true });
          }} className="flex min-h-80 flex-1 flex-col">
            <div className="flex items-center gap-2">
              {services.length === 1 ? <span className="font-medium">{focused.name}</span> : null}
              <TabsList variant="line">
                <TabsTrigger value="build" disabled={!build} title={build ? undefined : "Prebuilt image, nothing built"}>Build</TabsTrigger>
                <TabsTrigger value="deploy">Deploy</TabsTrigger>
              </TabsList>
              {services.length === 1 ? (
                <Badge variant={outcomeBadges[nodeLight(focused.outcome, deployment.status)]} className="ml-auto">{nodeStatusLabels[focused.outcome]}</Badge>
              ) : null}
            </div>
            <TabsContent value="build" className="mt-3 flex min-h-0 flex-1 flex-col gap-2">
              {build ? <>
                <p className="text-muted-foreground">
                  {build.status[0]?.toUpperCase()}{build.status.slice(1)} from {build.commit ? <span className="font-mono">{build.commit.slice(0, 7)}</span> : "the upload"}
                </p>
                {build.message ? <p className="break-words text-destructive">{build.message}</p> : null}
                <Suspense fallback={<Skeleton className="h-24 w-full" />}>
                  <StoreBuildLog deploymentId={deployment.id} service={build.service} />
                </Suspense>
              </> : null}
            </TabsContent>
            <TabsContent value="deploy" className="mt-3 flex min-h-0 flex-1 flex-col">
              <ContainerLogs selection={{ organizationSlug: params.organizationSlug, deploymentId: deployment.id, serviceId: focused.id }} />
            </TabsContent>
          </Tabs>
        ) : (
          <p className="text-muted-foreground">No service in this deployment.</p>
        )}

        {volumes.length ? (
          <section className="flex flex-col gap-2">
            <h3 className="font-medium">Volumes</h3>
            <ItemGroup className="gap-2">
              {volumes.map((node) => (
                <Item key={node.id} variant="outline" size="sm">
                  <ItemContent className="min-w-0"><ItemTitle><span className="truncate">{node.name}</span></ItemTitle></ItemContent>
                  <Badge variant={outcomeBadges[nodeLight(node.outcome, deployment.status)]}>{nodeStatusLabels[node.outcome]}</Badge>
                </Item>
              ))}
            </ItemGroup>
          </section>
        ) : null}
      </div>
    </div>
  );
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
 * yet, Cancel one before it ends. Whoever admitted it, CLI or dashboard: they share one queue per Environment.
 */
function StoreDeploymentActions({ deployment, focused }: { deployment: DeploymentView; focused: NodeOutcome | undefined }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const writer = useStoreWriter(params.organizationSlug);
  const [cancelOpen, setCancelOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const actions = deploymentActions(deployment.status);
  // With no Server, nothing can run it: the way on is adding one.
  const { noServers } = useRuntimeLens(params.organizationSlug);

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

  // A failed Deployment's focused Service it didn't apply can be fixed on a Branch, with the change that failed.
  const fixing = deployment.status === "failed" && focused && !nodeApplied(focused.outcome) ? focused.name : null;

  return (
    <div className="flex shrink-0 flex-wrap items-center gap-2">
      {fixing ? (
        <Button size="sm" variant="outline" nativeButton={false} render={<Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO}
          params={params} search={{ focus: fixing, fix: deployment.id }} />}>
          <GitBranchPlusIcon data-icon="inline-start" />Fix it on a branch
        </Button>
      ) : null}
      {noServers && (actions.retry || actions.start) ? (
        <Button size="sm" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/servers" params={params} />}>Add a server</Button>
      ) : null}
      {actions.retry && !noServers ? <Button size="sm" variant="outline" disabled={retrying} onClick={() => void retry()}>Retry</Button> : null}
      {actions.start && !noServers ? (
        <Button size="sm" variant="outline" onClick={() => { writer.commit({ command: "start", deployment: deployment.id }); }}>Deploy now</Button>
      ) : null}
      {actions.cancel ? <Button size="sm" variant="outline" onClick={() => setCancelOpen(true)}>Cancel</Button> : null}
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
    </div>
  );
}
