import { Suspense, type ReactNode } from "react";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { RelativeTime } from "#/components/relative-time";
import { ListRowSkeletons, ShowMore } from "#/components/show-more";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { useNodeDeployments } from "#/modules/deployments/deployment-history.queries";
import { useDeploymentAttempt, useDeploymentList } from "#/modules/deployments/deployment.collection";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { deploymentStatusLabel, nodeOutcomeLabels, shortDeploymentId, type DeploymentNodeView, type DeploymentViewStatus } from "#/modules/deployments/deployment-view";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "./deployment-page";
import { useEnvironmentNavigationNodes } from "./environment-node-navigation";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

/**
 * The Environment's Cloud Deployment Attempts, newest first, a server page at a time; each row opens its Deployment Page.
 * `service` narrows the list to the attempts that changed that service, each showing what happened to it.
 */
export function DeploymentsList({ service }: { service: string | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
  const services = useEnvironmentNavigationNodes(params).nodes.filter((node) => node.type === "service");
  const items = [{ value: null, label: "All services" }, ...services.map((node) => ({ value: node.id, label: node.name }))];

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}><span className="font-medium">Deployments</span></CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
        <Select items={items} value={service}
          onValueChange={(next) => void navigate({ to: ".", search: { service: next ?? undefined }, replace: true })}>
          <SelectTrigger size="sm" aria-label="Service"><SelectValue /></SelectTrigger>
          <SelectContent>
            {items.map((item) => <SelectItem key={item.value} value={item.value}>{item.label}</SelectItem>)}
          </SelectContent>
        </Select>
        <nav aria-label="Deployments">
          <ItemGroup className="gap-1">
            <Suspense fallback={<ListRowSkeletons />}>
              {service ? <ServiceRows service={service} /> : <AllRows />}
            </Suspense>
          </ItemGroup>
        </nav>
      </div>
    </div>
  );
}

function AllRows() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { attempts, hasMore, loadingMore, showMore } = useDeploymentList(params.organizationSlug, environmentId);
  return <Rows hasMore={hasMore} loading={loadingMore} onShowMore={showMore}>
    {attempts.map(({ deployment, view }) => {
      const ShownRow = isActiveDeployment(deployment.status) ? ActiveRow : Row;
      return <ShownRow key={deployment.id} deployment={deployment} status={view.status} label={deploymentStatusLabel(view)} />;
    })}
  </Rows>;
}

function ServiceRows({ service }: { service: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { data, hasNextPage, isFetchingNextPage, fetchNextPage } = useNodeDeployments(params.organizationSlug, environmentId, service);
  return <Rows hasMore={hasNextPage} loading={isFetchingNextPage} onShowMore={() => void fetchNextPage()}>
    {data.pages.flatMap((page) => page.items).map((deployment) => {
      const ShownRow = activeOutcomes.has(deployment.outcome) ? ActiveRow : Row;
      return <ShownRow key={deployment.id} deployment={deployment} status={deployment.outcome} label={nodeOutcomeLabels[deployment.outcome]} service={service} />;
    })}
  </Rows>;
}

function Rows({ children, hasMore, loading, onShowMore }: { children: ReactNode[]; hasMore: boolean; loading: boolean; onShowMore: () => void }) {
  return <>
    {children.length === 0 ? <Empty variant="placeholder"><EmptyDescription>No deployments yet</EmptyDescription></Empty> : children}
    <ShowMore hasMore={hasMore} loading={loading} onShowMore={onShowMore} />
  </>;
}

const activeOutcomes = new Set<DeploymentNodeView["outcome"]>(["queued", "building", "deploying"]);

type RowProps = {
  deployment: { id: string; message: string | null; createdAt: Date };
  status: DeploymentViewStatus | DeploymentNodeView["outcome"];
  label: string;
  service?: string;
};

/**
 * An active attempt's row reads its status as the bottom bar and its Deployment Page do, build tail included, so all three
 * name the same one (a queued attempt building its images reads Building). Until then it shows the listed status.
 */
function ActiveRow(props: RowProps) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { attempt } = useDeploymentAttempt(params.organizationSlug, environmentId, props.deployment.id, { buildLog: true });
  if (!attempt) return <Row {...props} />;
  if (!props.service) return <Row {...props} status={attempt.view.status} label={deploymentStatusLabel(attempt.view)} />;
  const node = attempt.view.nodes.find((candidate) => candidate.nodeId === props.service);
  return node ? <Row {...props} status={node.outcome} label={nodeOutcomeLabels[node.outcome]} /> : <Row {...props} />;
}

/** Filtered to a service, a row shows that service's Node Outcome and opens the page focused on it. */
function Row({ deployment, status, label, service }: RowProps) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  return (
    <Item size="sm" render={<Link to={DEPLOYMENT_PAGE_ROUTE_TO} params={{ ...params, deploymentId: deployment.id }} search={{ service }} />}>
      <ItemContent className="min-w-0">
        <ItemTitle className="w-full"><span className="truncate">{deployment.message ?? "Deployment"}</span></ItemTitle>
        <ItemDescription className="flex items-center gap-1.5 [&_svg]:size-3.5">
          <DeploymentStatusIcon status={status} />{label} · <span className="font-mono">{shortDeploymentId(deployment.id)}</span>
        </ItemDescription>
      </ItemContent>
      <ItemActions className="text-sm text-muted-foreground"><RelativeTime date={deployment.createdAt} /></ItemActions>
    </Item>
  );
}
