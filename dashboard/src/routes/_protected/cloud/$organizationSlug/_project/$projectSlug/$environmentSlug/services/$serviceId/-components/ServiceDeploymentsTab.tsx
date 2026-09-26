import { Suspense } from "react";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { Badge } from "#/components/ui/badge";
import { buttonVariants } from "#/components/ui/button-variants";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Skeleton } from "#/components/ui/skeleton";
import { TabsContent } from "#/components/ui/tabs";
import { ShowMore } from "#/components/show-more";
import { useNodeDeployments, type NodeDeployment } from "#/modules/deployments/deployment-history.queries";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { nodeOutcomeLabels, shortDeploymentId } from "#/modules/deployments/deployment-view";
import { RelativeTime } from "#/components/relative-time";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../../../-components/environment-route-paths";

function Summary({ deployment }: { deployment: NodeDeployment }) {
  return (
    <ItemContent className="min-w-0">
      <ItemTitle><span><span className="font-mono">{shortDeploymentId(deployment.id)}</span> · {deployment.message ?? "Deployment"}</span></ItemTitle>
      <ItemDescription><RelativeTime date={deployment.createdAt} /></ItemDescription>
    </ItemContent>
  );
}

/**
 * The live panel's Deployments tab: the attempt currently serving this service (the newest that
 * Deployed it, so a later failure leaves it in place) and the other attempts that changed it.
 * Each opens the canvas in Deployment Mode for that attempt with this service open.
 */
export function ServiceDeploymentsTab({ organizationSlug, serviceId }: { organizationSlug: string; serviceId: string }) {
  return (
    <TabsContent value="deployments" className="mt-4 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-4">
        <Suspense fallback={<Skeleton className="h-16 w-full" />}>
          <Deployments organizationSlug={organizationSlug} serviceId={serviceId} />
        </Suspense>
      </div>
    </TabsContent>
  );
}

function Deployments({ organizationSlug, serviceId }: { organizationSlug: string; serviceId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { data, hasNextPage, fetchNextPage, isFetchingNextPage } = useNodeDeployments(organizationSlug, environmentId, serviceId);
  const running = data.pages[0]?.running;
  // History holds the Running attempt too; it shows once, as Running.
  const history = data.pages.flatMap((page) => page.items).filter((deployment) => deployment.id !== running?.id);
  const open = (deployment: NodeDeployment, tab?: "deploy-logs") =>
    <Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId }} search={{ deployment: deployment.id, tab }} />;

  return (
    <>
      {running ? (
        // The card is one link (links cannot nest), so it goes where its "View logs" label says.
        <Item variant="outline" render={open(running, "deploy-logs")}>
          <Badge variant="success">Running</Badge>
          <Summary deployment={running} />
          <ItemActions><span className={buttonVariants({ variant: "outline", size: "sm" })}>View logs</span></ItemActions>
        </Item>
      ) : (
        <p className="text-muted-foreground">No deployment is serving this service yet.</p>
      )}
      <section className="flex flex-col gap-2">
        <h3 className="flex justify-between font-medium">History<span className="font-normal text-muted-foreground">Hiding unchanged</span></h3>
        {history.length > 0 ? (
          <ItemGroup className="gap-2">
            {history.map((deployment) => (
              <Item key={deployment.id} variant="outline" size="sm" render={open(deployment)}>
                <Badge variant={outcomeBadges[deployment.outcome]}>{nodeOutcomeLabels[deployment.outcome]}</Badge>
                <Summary deployment={deployment} />
              </Item>
            ))}
          </ItemGroup>
        ) : (
          <p className="text-muted-foreground">No other deployments changed this service.</p>
        )}
        <ShowMore hasMore={hasNextPage} loading={isFetchingNextPage} onShowMore={() => void fetchNextPage()} />
      </section>
    </>
  );
}
