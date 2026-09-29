import { Suspense, type ReactNode } from "react";
import type { ServiceListing } from "@ployz/sdk";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { ListRowSkeletons, ShowMore } from "#/components/show-more";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { deploymentStatusIcons, deploymentStatusLabels, targetsLabel, uploadLabel } from "#/modules/config-store/store-deployments";
import { requireView, servicesQuery, useStoreDeployments, useStoreView } from "#/modules/config-store/store-view.queries";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "./deployment-page";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

/** The Config Store's Deployments of the Environment, the CLI's and this dashboard's alike. */
export function DeploymentsList({ service }: { service: string | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const services = requireView(useStoreView(params.organizationSlug, servicesQuery(store))).services;
  return (
    <ListFrame service={service} services={services}>
      <StoreDeploymentRows service={services.find((candidate) => candidate.id === service) ?? null} />
    </ListFrame>
  );
}

function ListFrame({ service, services, children }: { service: string | null; services: { id: string; name: string }[]; children: ReactNode }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();
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
            <Suspense fallback={<ListRowSkeletons />}>{children}</Suspense>
          </ItemGroup>
        </nav>
      </div>
    </div>
  );
}

/**
 * Store Deployments newest first, a page at a time. Narrowed to a Service, the ones that deployed it: every full
 * Deploy, and targeted ones that named it. Each opens its Deployment Page, focused on that Service.
 */
// ponytail: filters loaded pages by the name the Service had when admitted; a renamed Service loses older rows, and a
// page can filter to nothing (Show more still loads the next). A Store query by Service when that matters.
export function StoreDeploymentRows({ service, returnTo }: { service: ServiceListing | null; returnTo?: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { data, hasNextPage, isFetchingNextPage, fetchNextPage } = useStoreDeployments(params.organizationSlug, store);
  const deployments = data.pages.flatMap((page) => page.deployments)
    .filter((deployment) => !service || deployment.services.length === 0 || deployment.services.includes(service.name));
  return <Rows hasMore={hasNextPage} loading={isFetchingNextPage} onShowMore={() => void fetchNextPage()}>
    {deployments.map((deployment) => (
      <Item key={deployment.id} size="sm" render={<Link to={DEPLOYMENT_PAGE_ROUTE_TO} params={{ ...params, deploymentId: deployment.id }}
        search={{ service: service?.id, returnTo }} />}>
        <ItemContent className="min-w-0">
          <ItemTitle className="w-full"><span className="truncate">Deployment #{deployment.number}</span></ItemTitle>
          <ItemDescription className="flex items-center gap-1.5 [&_svg]:size-3.5">
            <DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />{deploymentStatusLabels[deployment.status]}
            {" · "}<span className="truncate">{deployment.upload ? uploadLabel(deployment.upload) : `Deploys ${targetsLabel(deployment)}${deployment.admitted_by ? ` · by ${deployment.admitted_by}` : ""}`}</span>
          </ItemDescription>
        </ItemContent>
      </Item>
    ))}
  </Rows>;
}

function Rows({ children, hasMore, loading, onShowMore }: { children: ReactNode[]; hasMore: boolean; loading: boolean; onShowMore: () => void }) {
  return <>
    {children.length === 0 ? <Empty variant="placeholder"><EmptyDescription>No deployments yet</EmptyDescription></Empty> : children}
    <ShowMore hasMore={hasMore} loading={loading} onShowMore={onShowMore} />
  </>;
}
