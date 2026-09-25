import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { Badge } from "#/components/ui/badge";
import { buttonVariants } from "#/components/ui/button-variants";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { TabsContent } from "#/components/ui/tabs";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { useNodeDeployments } from "#/modules/deployments/deployment.collection";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { nodeOutcomeLabels, shortDeploymentId } from "#/modules/deployments/deployment-view";
import { formatRelativeTime } from "#/utils/relative-time";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../../../-components/environment-route-paths";

function Summary({ deployment }: { deployment: EnvironmentDeploymentSummary }) {
  return (
    <ItemContent className="min-w-0">
      <ItemTitle className="w-full truncate"><span className="font-mono">{shortDeploymentId(deployment.id)}</span> · {deployment.message ?? "Deployment"}</ItemTitle>
      <ItemDescription>{formatRelativeTime(deployment.createdAt)}</ItemDescription>
    </ItemContent>
  );
}

/**
 * The live panel's Deployments tab: the attempt currently serving this service (the newest that
 * Deployed it, so a later failure leaves it in place) and the other attempts that changed it.
 * Each opens the canvas in Deployment Mode for that attempt with this service open.
 */
export function ServiceDeploymentsTab({ organizationSlug, serviceId }: { organizationSlug: string; serviceId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const attempts = useNodeDeployments(organizationSlug, environmentId, serviceId);
  const running = attempts.find(({ node }) => node.outcome === "deployed");
  const history = attempts.filter((attempt) => attempt !== running && attempt.node.outcome !== "unchanged");
  const open = (deployment: EnvironmentDeploymentSummary, tab?: "deploy-logs") =>
    <Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId }} search={{ deployment: deployment.id, tab }} />;

  return (
    <TabsContent value="deployments" className="mt-4 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-4">
        {running ? (
          // The card is one link (links cannot nest), so it goes where its "View logs" label says.
          <Item state="success" render={open(running.deployment, "deploy-logs")}>
            <Badge variant="success">Running</Badge>
            <Summary deployment={running.deployment} />
            <ItemActions><span className={buttonVariants({ variant: "outline", size: "sm" })}>View logs</span></ItemActions>
          </Item>
        ) : (
          <p className="text-sm text-muted-foreground">No deployment is serving this service yet.</p>
        )}
        <section className="flex flex-col gap-2">
          <h3 className="flex justify-between text-xs text-muted-foreground uppercase">History<span className="normal-case">Hiding unchanged</span></h3>
          {history.length > 0 ? (
            <ItemGroup className="gap-2">
              {history.map(({ deployment, node }) => (
                <Item key={deployment.id} variant="outline" size="sm" render={open(deployment)}>
                  <Badge variant={outcomeBadges[node.outcome]}>{nodeOutcomeLabels[node.outcome]}</Badge>
                  <Summary deployment={deployment} />
                </Item>
              ))}
            </ItemGroup>
          ) : (
            <p className="text-sm text-muted-foreground">No other deployments changed this service.</p>
          )}
        </section>
      </div>
    </TabsContent>
  );
}
