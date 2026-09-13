import { useCollectionScope } from "#/collections/use-collection-scope";
import { getEnvironmentNodeConfigSnapshotsCollection } from "#/collections/collections";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useParams } from "@tanstack/react-router";
import { DeploymentRow } from "#/components/deployment-row";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { TabsContent } from "#/components/ui/tabs";
import { useDeploymentsCollection } from "#/modules/services/services.collection";
import { ENVIRONMENT_ROUTE_FROM } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";

export function ServiceDeploymentsTab() {
  const { organizationSlug, projectSlug, environmentSlug } = useParams({
    from: ENVIRONMENT_ROUTE_FROM,
  });
  const deployments = useDeploymentsCollection(organizationSlug);

  const { serviceId } = useParams({ strict: false });
  const snapshots = getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ deployment: deployments })
        .innerJoin({ snapshot: snapshots }, ({ deployment, snapshot }) => eq(deployment.id, snapshot.environmentDeploymentId))
        .where(({ snapshot }) => eq(snapshot.nodeId, serviceId ?? ""))
        .where(({ snapshot }) => eq(snapshot.nodeType, "service"))
        .where(({ deployment }) => eq(deployment.projectSlug, projectSlug))
        .where(({ deployment }) =>
          eq(deployment.environmentSlug, environmentSlug),
        )
        .select(({ deployment }) => deployment),
  });

  const sorted = [...rows].sort(
    (left, right) => right.createdAt.getTime() - left.createdAt.getTime(),
  );

  return (
    <TabsContent value="deployments" className="mt-4 flex flex-col gap-4">
      {sorted.length === 0 ? (
        <Alert>
          <AlertTitle>No deployments yet</AlertTitle>
          <AlertDescription>
            Deploy this service to see its history here.
          </AlertDescription>
        </Alert>
      ) : (
        sorted.map((deployment) => (
          <DeploymentRow key={deployment.id} deployment={deployment} serviceId={serviceId} />
        ))
      )}
    </TabsContent>
  );
}
