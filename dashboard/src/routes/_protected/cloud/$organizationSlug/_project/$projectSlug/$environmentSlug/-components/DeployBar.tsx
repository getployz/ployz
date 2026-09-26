import { createContext, useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { Link, useLoaderData, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import {
  ChevronDownIcon, ChevronRightIcon, CircleCheckIcon, CircleDashedIcon, CircleDotIcon, CircleSlashIcon, CircleXIcon, PencilIcon,
} from "lucide-react";
import { CancelDeploymentDialog } from "#/components/cancel-deployment-dialog";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Drawer, DrawerContent, DrawerTitle, DrawerTrigger } from "#/components/ui/drawer";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemSeparator, ItemTitle } from "#/components/ui/item";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { useIsMobile } from "#/hooks/use-mobile";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { useDeployQueuedNow, useRetryDeployment } from "#/modules/deployments/deployment-commands";
import { useEnvironmentDeployments, type DeploymentAttempt } from "#/modules/deployments/deployment.collection";
import { deploymentStatusLabel, shortDeploymentId, type DeploymentView } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { RelativeTime } from "#/components/relative-time";
import { CANVAS_ROUTE_ID, useDeploymentMode } from "./deployment-mode";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";

function StatusIcon({ view }: { view: DeploymentView }) {
  switch (view.status) {
    case "deployed": return <CircleCheckIcon className="text-success" />;
    case "failed": return <CircleXIcon className="text-destructive" />;
    case "cancelled": return <CircleSlashIcon className="text-muted-foreground" />;
    case "queued": return <CircleDashedIcon className="text-muted-foreground" />;
    default: return <CircleDotIcon className="text-info" />;
  }
}

/** The editor canvas owns the change state, so it portals the apply zone into this slot of the bar. */
export const ApplyZoneSlot = createContext<HTMLElement | null>(null);

/**
 * The floating deploy bar: Editor | Deployments ⌄ on every screen size, usable while a service panel is open.
 * `children` renders after the segments (the apply zone, #1051).
 */
export function DeployBar({ children }: { children?: ReactNode }) {
  const { organizationSlug, environmentSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const listOpen = useSearch({ from: CANVAS_ROUTE_ID, select: (search) => search.deploymentList === true });
  const viewed = useDeploymentMode();
  const attempts = useEnvironmentDeployments(organizationSlug, environmentId);
  const { selectedNodeId } = useCanvasInspectorSelection();
  const navigate = useNavigate();
  const isMobile = useIsMobile();
  const barRef = useRef<HTMLDivElement>(null);
  // The oldest queued or running attempt holds, or is next for, the Environment execution slot; the rest wait behind it.
  const active = attempts.filter(({ deployment }) => isActiveDeployment(deployment.status));
  const running = active.at(-1);
  const queued = active.length > 1 ? active[0] : undefined;

  function setListOpen(open: boolean) {
    void navigate({ to: ".", search: (previous) => ({ ...previous, deploymentList: open || undefined }), replace: true });
  }

  // Esc closes the topmost thing: the list, menus and dialogs close themselves (they portal outside the scene), the panel closes itself, then Esc leaves Deployment Mode.
  useEffect(() => {
    if (!viewed || listOpen || selectedNodeId) return;
    function leaveOnEscape(event: KeyboardEvent) {
      if (event.key !== "Escape" || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
      const scene = barRef.current?.closest(".environment-canvas-scene");
      if (event.target !== document.body && !(event.target instanceof Node && scene?.contains(event.target))) return;
      void navigate({ to: ".", search: (previous) => ({ ...previous, deployment: undefined }) });
    }
    document.addEventListener("keydown", leaveOnEscape);
    return () => document.removeEventListener("keydown", leaveOnEscape);
  }, [viewed, listOpen, selectedNodeId, navigate]);

  const listTrigger = (
    <Button size="sm" variant="ghost" data-active={viewed !== null}
      aria-label={viewed ? `Deployment ${shortDeploymentId(viewed.deployment.id)}, all deployments` : "Deployments"}>
      {viewed ? <><StatusIcon view={viewed.view} /><span className="font-mono">{shortDeploymentId(viewed.deployment.id)}</span></>
        : "Deployments"}
      <ChevronDownIcon />
    </Button>
  );
  // A queued or running attempt opens directly from Editor Mode, with no list.
  const openRunning = !viewed && running ? (
    <Link to="." search={(previous) => ({ ...previous, deployment: running.deployment.id, deploymentList: undefined })}
      className={buttonVariants({ size: "sm", variant: "secondary" })}>
      <StatusIcon view={running.view} />
      <span className="tabular-nums">{running.view.status === "deploying" ? `Deploying ${running.view.deployed}/${running.view.changed}` : deploymentStatusLabel(running.view)}</span>
      <ChevronRightIcon />
    </Link>
  ) : null;
  const openQueued = !viewed && queued ? (
    <Link to="." search={(previous) => ({ ...previous, deployment: queued.deployment.id, deploymentList: undefined })}
      className={buttonVariants({ size: "sm", variant: "ghost" })}>
      <CircleDashedIcon />{active.length > 2 ? `${active.length - 1} queued` : "Queued"}
    </Link>
  ) : null;
  const list = <DeploymentList attempts={attempts} viewedId={viewed?.deployment.id ?? null} environmentSlug={environmentSlug} />;

  return (
    <div ref={barRef} role="group" aria-label="Deploy bar" className="deploy-bar" data-deployment={viewed ? "" : undefined}>
      {/* One toggle: the Editor or a deployment; the active segment is raised out of the track. */}
      <div className="flex min-w-0 items-center gap-0.5 rounded-lg bg-muted p-0.5 [&>[data-active=true]]:bg-background [&>[data-active=true]]:shadow-sm">
        <Link to="." search={(previous) => ({ ...previous, deployment: undefined, deploymentList: undefined })}
          className={buttonVariants({ size: "sm", variant: "ghost" })} data-active={viewed === null}>
          {/* Intent Pink marks staged changes; styles.css shows it only while the bar holds the apply zone. */}
          <span aria-hidden className="deploy-bar-pending size-2 rounded-full bg-changed" />
          Editor
        </Link>
        {openRunning}
        {openQueued}
        {isMobile ? (
          <Drawer open={listOpen} onOpenChange={setListOpen} showSwipeHandle>
            {openRunning ? null : <DrawerTrigger render={listTrigger} />}
            <DrawerContent><DrawerTitle className="sr-only">Deployments</DrawerTitle><div className="p-4">{list}</div></DrawerContent>
          </Drawer>
        ) : (
          <Popover open={listOpen} onOpenChange={setListOpen}>
            {openRunning ? null : <PopoverTrigger render={listTrigger} />}
            <PopoverContent anchor={barRef} side="top" sideOffset={8} className="w-[min(26rem,calc(100vw-2rem))]">
              <PopoverTitle className="sr-only">Deployments</PopoverTitle>
              {list}
            </PopoverContent>
          </Popover>
        )}
      </div>
      {viewed ? <DeploymentActions deployment={viewed.deployment}
        building={attempts.some(({ deployment }) => deployment.status === "queued" && deployment.inngestRunId !== null && deployment.id !== viewed.deployment.id)} /> : null}
      {children}
    </div>
  );
}

/**
 * Retry on a failed attempt, Deploy now on one queued for the next trigger, Cancel on a queued or running one; all keep their existing semantics.
 * `building`: another queued attempt is building, so this one is the pending attempt and waits for it rather than offering Deploy now.
 */
function DeploymentActions({ deployment, building }: { deployment: EnvironmentDeploymentSummary; building: boolean }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const [retry, isRetrying] = useRetryDeployment(deployment);
  const [deployNow, isDispatching] = useDeployQueuedNow(deployment);
  const [cancelOpen, setCancelOpen] = useState(false);
  const cancellable = !deployment.cancellationRequestedAt && isActiveDeployment(deployment.status);
  return <>
    {deployment.canRetry ? <Button size="sm" variant="outline" disabled={isRetrying} onClick={() => void retry()}>Retry</Button> : null}
    {/* Queued with no dispatch requested: it waits for the environment's next trigger. */}
    {deployment.status === "queued" && !deployment.dispatchRequestedAt ? (building
      ? <span className="text-sm text-muted-foreground">Waiting for the current build</span>
      : <Button size="sm" variant="outline" disabled={isDispatching} onClick={() => void deployNow()}>Deploy now</Button>) : null}
    {cancellable ? <Button size="sm" variant="outline" onClick={() => setCancelOpen(true)}>Cancel</Button> : null}
    <CancelDeploymentDialog open={cancelOpen} onOpenChange={setCancelOpen} organizationSlug={organizationSlug} deployment={deployment} />
  </>;
}

/** The Editor first, then the environment's deployments newest first. */
function DeploymentList({ attempts, viewedId, environmentSlug }: { attempts: DeploymentAttempt[]; viewedId: string | null; environmentSlug: string }) {
  return (
    <nav aria-label="Deployments" className="max-h-[min(28rem,70dvh)] overflow-y-auto"><ItemGroup>
      <ListRow current={viewedId === null} search={{ deployment: undefined }}
        icon={<PencilIcon />} title="Editor" detail={`${environmentSlug} as it is now`} />
      <ItemSeparator />
      {attempts.length === 0 ? <Empty variant="placeholder"><EmptyDescription>No deployments yet</EmptyDescription></Empty> : null}
      {attempts.map(({ deployment, view }) => (
        <ListRow key={deployment.id} current={deployment.id === viewedId} search={{ deployment: deployment.id }}
          icon={<StatusIcon view={view} />}
          title={<><span className="font-mono">{shortDeploymentId(deployment.id)}</span> · {deployment.message ?? "Deployment"}</>}
          detail={<>{deploymentStatusLabel(view)} · <RelativeTime date={deployment.createdAt} /></>} />
      ))}
    </ItemGroup></nav>
  );
}

function ListRow({ current, search, icon, title, detail }: {
  current: boolean; search: { deployment: string | undefined }; icon: ReactElement; title: ReactNode; detail: ReactNode;
}) {
  return (
    <Item size="xs" variant={current ? "muted" : "default"} data-current={current}
      render={<Link to="." search={(previous) => ({ ...previous, ...search, deploymentList: undefined })} />}>
      <ItemMedia variant="icon">{icon}</ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle><span>{title}</span></ItemTitle>
        <ItemDescription className="truncate">{detail}</ItemDescription>
      </ItemContent>
    </Item>
  );
}
