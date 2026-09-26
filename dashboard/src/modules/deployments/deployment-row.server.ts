import "@tanstack/react-start/server-only";
import { getTableColumns, inArray, or, sql } from "drizzle-orm";
import { volumeRemoveAttempt } from "#/modules/runtime/tables";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "./runtime-contract";
import { environmentDeployment, environmentDeploymentEvent } from "./tables";

// The plan the runtime executes (manifest, producers, action policy) stays on the server; no view reads it.
const { deployManifest: _manifest, variableProducers: _producers, serviceActionPolicy: _policy, ...deploymentColumns } =
  getTableColumns(environmentDeployment);

/** The deployment row every browser read returns: the Org Store collection and the history reads map it the same way. */
export const deploymentRowColumns = {
  ...deploymentColumns,
  runtimeProgress: sql<typeof environmentDeployment.$inferSelect.runtimeProgress>`coalesce(
    ${environmentDeployment.runtimeProgress},
    (select progress from ${environmentDeploymentEvent}
     where deployment_id = ${environmentDeployment}.${sql.identifier("id")} order by id desc limit 1)
  )`,
  // Removal retries need a fresh destructive review, so an attempt that staged one cannot retry.
  canRetry: sql<boolean>`${environmentDeployment.status} = 'failed' and not exists (
    select 1 from ${volumeRemoveAttempt}
    where ${volumeRemoveAttempt.environmentDeploymentId} = ${environmentDeployment}.${sql.identifier("id")}
  )`,
};

/**
 * The deployment rows the Org Store holds: active attempts plus the latest attempt per Environment.
 * Older attempts come from the paged history reads.
 */
export const orgStoreDeploymentSlice = (organizationId: string) => or(
  inArray(environmentDeployment.status, [...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES]),
  sql`${environmentDeployment.id} in (
    select distinct on (latest.environment_id) latest.id from ${environmentDeployment} latest
    where latest.organization_id = ${organizationId}
    order by latest.environment_id, latest.created_at desc, latest.id desc
  )`,
);
