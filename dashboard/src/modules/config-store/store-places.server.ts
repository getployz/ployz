import "@tanstack/react-start/server-only";
import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";

/** Where an Environment sits: its name and its Project's. Null when the Environment is gone. */
export const environmentPlace = Effect.fn("StorePlaces.environmentPlace")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [place] = yield* drizzle.execute<{ environment: string; project: string }>(sql`
    select e.name as environment, p.name as project
    from config_environment e join config_project p on p.id = e.project_id
    where e.id = ${environmentId}`, "objects");
  return place ?? null;
});

/**
 * Where a Service sits, from the Namespace its Engine name carries: the Project and Environment names and the slug the
 * Store applied it under. `slug` is null when nothing applied it. Null when no Environment holds the Namespace.
 */
export const placeService = Effect.fn("StorePlaces.placeService")(function* (organizationId: string, namespace: string, serviceId: string) {
  const { drizzle } = yield* Database;
  const [place] = yield* drizzle.execute<{ project: string; environment: string; slug: string | null }>(sql`
    select p.name as project, e.name as environment, (a.node::jsonb)->>'slug' as slug
    from config_namespace n
      join config_environment e on e.id = n.environment_id
      join config_project p on p.id = e.project_id
      left join config_applied a on a.environment_id = e.id and a.node_id = ${serviceId} and a.node_type = 'service'
    where n.organization_id = ${organizationId} and n.namespace = ${namespace}`, "objects");
  return place ?? null;
});

/** Each Service an Environment last applied, with the node it was applied as and the Cluster Domain that Deployment froze. */
export const appliedServices = Effect.fn("StorePlaces.appliedServices")(function* (organizationId: string, environmentId: string) {
  const { drizzle } = yield* Database;
  return yield* drizzle.execute<{ serviceId: string; node: string; clusterDomain: string | null }>(sql`
    select a.node_id as "serviceId", a.node, d.cluster_domain as "clusterDomain"
    from config_applied a join config_deployment d on d.id = a.deployment_id
    where a.environment_id = ${environmentId} and a.organization_id = ${organizationId} and a.node_type = 'service'
  `, "objects");
});

/** The ids of the Services `deploymentId` applied in the Environment, as a subquery to use in a `where ... in`. */
export const servicesAppliedBy = (environmentId: string, deploymentId: string): SQL => sql`(select node_id from config_applied
  where environment_id = ${environmentId} and deployment_id = ${deploymentId} and node_type = 'service')`;

/** Every Environment that has an applied Service. */
export const environmentsWithServices = Effect.fn("StorePlaces.environmentsWithServices")(function* () {
  const { drizzle } = yield* Database;
  return yield* drizzle.execute<{ organizationId: string; environmentId: string }>(sql`
    select distinct organization_id as "organizationId", environment_id as "environmentId"
    from config_applied where node_type = 'service'
  `, "objects");
});

/** Holds off the Environment's delete until the transaction ends. False when the Organization has no such Environment. */
export const holdEnvironment = Effect.fn("StorePlaces.holdEnvironment")(function* (organizationId: string, environmentId: string) {
  const { drizzle } = yield* Database;
  const [environment] = yield* drizzle.execute<{ id: string }>(sql`
    select id from config_environment where id = ${environmentId} and organization_id = ${organizationId}
    for key share`, "objects");
  return environment !== undefined;
});
