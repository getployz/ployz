import { createContext, Suspense, useRef, type ReactNode } from "react";
import { Link, useLoaderData, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { ChevronDownIcon, ChevronRightIcon, CircleDashedIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Drawer, DrawerContent, DrawerTitle, DrawerTrigger } from "#/components/ui/drawer";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { ListRowSkeletons, ShowMore } from "#/components/show-more";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useIsMobile } from "#/hooks/use-mobile";
import { useDeploymentList, useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { environmentDeploymentsQueryOptions } from "#/modules/deployments/deployment-history.queries";
import { deploymentStatusLabel, shortDeploymentId } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { RelativeTime } from "#/components/relative-time";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "./deployment-page";
import { useCanvasInspectorSelection } from "./useCanvasInspectorSelection";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

const CANVAS_ROUTE_ID = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas";

/** The editor canvas owns the change state, so it portals the apply zone into this slot of the bar. */
export const ApplyZoneSlot = createContext<HTMLElement | null>(null);

/**
 * The floating deploy bar, usable while a panel is open: the running deployment's shortcut or the deployment list,
 * each opening a Deployment Page. `children` renders after them (the apply zone, #1051).
 */
export function DeployBar({ children }: { children?: ReactNode }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const listOpen = useSearch({ from: CANVAS_ROUTE_ID, select: (search) => search.deploymentList === true });
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const attempts = useEnvironmentDeployments(params.organizationSlug, environmentId);
  const navigate = useNavigate();
  const isMobile = useIsMobile();
  const { queryClient } = useCollectionScope();
  const warmList = () => void queryClient.prefetchInfiniteQuery(environmentDeploymentsQueryOptions(params.organizationSlug, environmentId));
  const barRef = useRef<HTMLDivElement>(null);
  // The oldest queued or running attempt holds, or is next for, the Environment execution slot; the rest wait behind it.
  const active = attempts.filter(({ deployment }) => isActiveDeployment(deployment.status));
  const running = active.at(-1);
  const queued = active.length > 1 ? active[0] : undefined;
  const page = (deploymentId: string) => ({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId } }) as const;

  function setListOpen(open: boolean) {
    void navigate({ to: ".", search: (previous) => ({ ...previous, deploymentList: open || undefined }), replace: true });
  }

  const listTrigger = (
    <Button size="sm" variant="ghost" onPointerEnter={warmList} onFocus={warmList}>
      Deployments<ChevronDownIcon />
    </Button>
  );
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
  const list = <DeploymentList organizationSlug={params.organizationSlug} environmentId={environmentId} viewedId={viewedId} />;

  return (
    <div ref={barRef} role="group" aria-label="Deploy bar" className="deploy-bar">
      <div className="flex min-w-0 items-center gap-0.5">
        {/* Intent Pink marks staged changes; styles.css shows it only while the bar holds the apply zone. */}
        <span aria-hidden className="deploy-bar-pending mx-1.5 size-2 rounded-full bg-changed" />
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
      {children}
    </div>
  );
}

/** The environment's deployments newest first, a page at a time; each opens its Deployment Page. */
function DeploymentList({ organizationSlug, environmentId, viewedId }: { organizationSlug: string; environmentId: string; viewedId: string | null }) {
  return (
    <nav aria-label="Deployments" className="max-h-[min(28rem,70dvh)] overflow-y-auto"><ItemGroup>
      <Suspense fallback={<ListRowSkeletons />}>
        <DeploymentRows organizationSlug={organizationSlug} environmentId={environmentId} viewedId={viewedId} />
      </Suspense>
    </ItemGroup></nav>
  );
}

function DeploymentRows({ organizationSlug, environmentId, viewedId }: { organizationSlug: string; environmentId: string; viewedId: string | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { attempts, hasMore, loadingMore, showMore } = useDeploymentList(organizationSlug, environmentId);
  return <>
    {attempts.length === 0 ? <Empty variant="placeholder"><EmptyDescription>No deployments yet</EmptyDescription></Empty> : null}
    {attempts.map(({ deployment, view }) => {
      const current = deployment.id === viewedId;
      return (
        <Item key={deployment.id} size="xs" variant={current ? "muted" : "default"} data-current={current}
          render={<Link to={DEPLOYMENT_PAGE_ROUTE_TO} params={{ ...params, deploymentId: deployment.id }} aria-current={current ? "page" : undefined} />}>
          <ItemMedia variant="icon"><DeploymentStatusIcon status={view.status} /></ItemMedia>
          <ItemContent className="min-w-0">
            <ItemTitle><span><span className="font-mono">{shortDeploymentId(deployment.id)}</span> · {deployment.message ?? "Deployment"}</span></ItemTitle>
            <ItemDescription className="truncate">{deploymentStatusLabel(view)} · <RelativeTime date={deployment.createdAt} /></ItemDescription>
          </ItemContent>
        </Item>
      );
    })}
    <ShowMore hasMore={hasMore} loading={loadingMore} onShowMore={showMore} />
  </>;
}
