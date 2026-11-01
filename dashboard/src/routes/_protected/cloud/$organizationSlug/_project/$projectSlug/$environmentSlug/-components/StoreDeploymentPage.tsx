import { useState, type ReactNode } from "react";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import type { DeploymentView, DeploymentWarning } from "@ployz/sdk";
import { ArrowLeftIcon, GitBranchPlusIcon, LockIcon, MoreVerticalIcon, TriangleAlertIcon } from "lucide-react";
import { toast } from "sonner";
import { DeletionDialog } from "#/components/deletion-dialog";
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
  admission, canFixOnBranch, deploymentActions, deploymentWarningText, deploymentByline, deploymentLabel, deploymentLight, deploymentStatusLabel, freshRollback, nodeApplied,
  outcomeReason, rollbackConfirm, showCancelling, shownNodeStatus, warnedService, type DeploymentAction,
} from "#/modules/config-store/store-deployments";
import { deploymentQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreCancelling } from "#/modules/config-store/store-pending";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useVolumeLossCheck, type VolumeLossCheck } from "#/modules/config-store/use-volume-loss-check";
import { useCopiesWait, useStorageSteps } from "#/modules/volume-placement/volume-placement.hooks";
import { useDeploymentCleanup } from "#/modules/config-store/retire-old.queries";
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
  const cleanup = useDeploymentCleanup(organizationSlug, deploymentId, result.ok ? result.value : undefined).data ?? null;
  if (!result.ok) return <Empty variant="placeholder" className="m-4"><EmptyDescription>Deployment not found</EmptyDescription></Empty>;
  const deployment = showCancelling(result.value, cancelling);
  const cancel = () => void writer.commit({ command: "cancel", deployment: deployment.id });
  const graph = makeRunGraph(deployment, cleanup);
  const selected = selectRunNode(graph, search);
  return (
    // Stacked when the page is narrow, on a phone or beside the Agent, it scrolls. The canvas keeps its height and the panel gets a screen of its own.
    <div className="flex min-h-0 flex-1 gap-2 p-2 @max-5xl:flex-col @max-5xl:overflow-y-auto">
      <div className="relative min-h-80 flex-1 overflow-hidden rounded-xl border @max-5xl:h-3/5 @max-5xl:min-h-96 @max-5xl:flex-none">
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
  const deployWarnings = warnings.filter((warning) => "warning" in warning);
  const variableWarnings = warnings.filter((warning) => !("ingress" in warning) && !("cause" in warning) && !("warning" in warning));
  const services = deployment.nodes.filter((node) => node.type === "service");
  const service = selected?.type === "build" ? services.find((node) => node.name === selected.name)
    : selected && "node" in selected && selected.node.type === "service" ? selected.node
    : services.find((node) => shownNodeStatus(node) === "failed") ?? services.find((node) => !nodeApplied(node.outcome));
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
            <DeploymentStatusIcon status={deploymentLight(deployment)} />
            <h1 className="truncate text-2xl font-semibold">
              {deployment.message ? `${deploymentLabel(deployment)} · ${deployment.message}` : `Deployment ${deploymentLabel(deployment)}`}
            </h1>
          </span>
          <p className="truncate text-xs text-muted-foreground">
            {deploymentStatusLabel(deployment)}
            {/* One that ended as it started has no duration to show. */}
            {started && ended?.getTime() !== started.getTime() ? (
              <>{" · "}<span className="tabular-nums">
                {ended ? formatDuration((ended.getTime() - started.getTime()) / 1000) : <RunningTime from={started} />}
              </span></>
            ) : null}
            {graph.total ? ` · ${graph.done} of ${graph.total}` : null}
          </p>
          <p className="break-words text-xs text-muted-foreground">
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

      <VariableWarnings warnings={variableWarnings} />
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
      {deployWarnings.map((warning) => {
        const node = warnedService(deployment, warning);
        const text = deploymentWarningText(warning, node?.name);
        return (
          <p key={text} role="status" className="flex flex-wrap items-center gap-1.5 text-sm text-warning">
            <TriangleAlertIcon className="size-4" />{text}. Set the domain port or PORT.
            {node ? (
              <Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId: node.id }} search={{}}
                className="underline underline-offset-4">Service settings</Link>
            ) : null}
          </p>
        );
      })}
    </div>
  );
}

/** What a Deployment shipped despite: a secret or broken reference left out, or a Config file reference left empty. */
export function VariableWarnings({ warnings }: { warnings: readonly DeploymentWarning[] }) {
  if (!warnings.length) return null;
  return (
    <ul aria-label="Warnings" className="flex flex-wrap gap-1.5">
      {warnings.map((warning) => (
        <li key={deploymentWarningText(warning)}>
          {/* Wraps: a sealed reference's hint runs past the card, and its ways out are at the end. */}
          <Badge variant="warning" className="h-auto items-start whitespace-normal text-left [&>svg]:mt-0.5 [&>svg]:shrink-0">
            {"broken" in warning && warning.broken ? <TriangleAlertIcon /> : <LockIcon />}
            {deploymentWarningText(warning)}
          </Badge>
        </li>
      ))}
    </ul>
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

/** The Volumes a redeploy accepts losing, at the version the Store asked about (none before it asked). */
type Acceptance = { accept: readonly string[]; version: string | null };

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
  const [rollbackOpen, setRollbackOpen] = useState(false);
  const [redeploying, setRedeploying] = useState(false);
  // What it rolls back as the Store last said, once that changed after the page loaded.
  const [fresh, setFresh] = useState<readonly number[] | null>(null);
  const [loss, setLoss] = useState<VolumeLossCheck | null>(null);
  const lossOf = useVolumeLossCheck(params.organizationSlug);
  const { primary, cancelInMenu } = deploymentActions(deployment, noServers);
  const waiting = deployment.status === "waiting_for_ci";
  const rollsBack = fresh ?? deployment.rolls_back;
  const rollback = rollbackConfirm({ number: deployment.number, rolls_back: [...rollsBack] });
  const copiesWait = useCopiesWait(params.organizationSlug, useLoaderData({ from: ENVIRONMENT_ROUTE_FROM }).environmentId);

  /**
   * This modal is the person's consent to roll back exactly what it named, so no Approval asks again; the Store refuses
   * when that changed, and the modal asks again. Deleting data a newer Deployment brought back asks by name. Resolves
   * with the Volume loss to ask about, if any.
   */
  async function redeploy(acceptance: Acceptance = { accept: [], version: null }): Promise<VolumeLossCheck | null> {
    const id = crypto.randomUUID();
    setRedeploying(true);
    try {
      await writer.commit({
        command: "admit", admit: "retry", id, deployment: deployment.id, rolls_back: [...rollsBack],
        version: acceptance.version, accept_volume_loss: [...acceptance.accept],
      }, ["confirmation_required"]).isPersisted.promise;
      void navigate(deploymentDestination(params, id, { job: search.job, origin: search.origin }));
      return null;
    } catch (error) {
      if (!(error instanceof StoreRefused)) return null;
      const check = lossOf(error);
      if (check) return check;
      const now = freshRollback(error);
      if (now) {
        setFresh(now);
        if (now.length > 0) setRollbackOpen(true);
        else toast("It rolls nothing back now. Redeploy again.");
      }
      return null;
    } finally {
      setRedeploying(false);
    }
  }

  async function confirmed() {
    setLoss(await redeploy());
  }

  // Deploy now starts a queued one while nothing else runs.
  const buttons = {
    add_server: <Button size="sm" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/servers" params={params} />}>Add a server</Button>,
    redeploy: (
      <RedeployAction copiesWait={copiesWait} disabled={redeploying}
        onRedeploy={() => (rollback ? setRollbackOpen(true) : void confirmed())} />
    ),
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
      {rollback ? (
        <AlertDialog open={rollbackOpen} onOpenChange={setRollbackOpen}>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>{rollback.title}</AlertDialogTitle>
              <AlertDialogDescription>{rollback.body}</AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <Button onClick={() => {
                setRollbackOpen(false);
                void confirmed();
              }}>Roll back</Button>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      ) : null}
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => { if (!open) setLoss(null); }}
        title="Deletes data"
        place={`${deployment.environment.project}/${deployment.environment.name}`}
        confirmLabel={rollback ? "Roll back" : "Redeploy"}
        items={loss?.items}
        callbacks={{
          load: () => Promise.resolve(loss ?? { items: [], evidence: { accept: [], version: "" }, losesWrites: false }),
          // Servers holding more by now: the Store asks again, and so does the dialog.
          confirm: async (evidence) => (await redeploy(evidence)) ?? undefined,
        }}
      />
      <AlertDialog open={cancelOpen} onOpenChange={setCancelOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{waiting ? "Cancel this push?" : "Cancel deployment?"}</AlertDialogTitle>
            <AlertDialogDescription>
              {waiting ? "It won't deploy."
                : storage ? "The step under way finishes first. The rest waits for your next deploy."
                : deployment.status === "queued"
                ? "It leaves the queue and never runs."
                : "It stops at its next step. What it already changed stays until the next Deploy."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{waiting ? "Keep waiting" : "Keep deploying"}</AlertDialogCancel>
            <Button variant="destructive" onClick={() => {
              setCancelOpen(false);
              onCancel();
            }}>{waiting ? "Cancel push" : "Cancel deployment"}</Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
