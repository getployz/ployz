import "@tanstack/react-start/server-only";
import type { ChangeSources } from "./change-log/declare";
import type { Projection } from "./change-log/projection.server";
import type { CollectionName } from "./read.contract";
import { canvasPositionsProjection } from "#/modules/canvas/canvas.collection.server";
import { clusterDomainProjection } from "#/modules/cluster-domain/cluster-domain.collection.server";
import { clusterNameProjection } from "#/modules/cluster-name/cluster-name.collection.server";
import { organizationEnrollmentProjection } from "#/modules/machines/enrollment.collection.server";
import { serverUpgradeSettingsProjection } from "#/modules/server-upgrade/server-upgrade.collection.server";
import { supportAccessProjection } from "#/modules/support-access/support-access.collection.server";

/** How each collection of `registry.ts` reads its rows. A collection without one doesn't compile. */
export const projections = {
  environment_canvas_node_position: canvasPositionsProjection,
  organization_enrollment: organizationEnrollmentProjection,
  organization_cluster_domain: clusterDomainProjection,
  organization_cluster_name: clusterNameProjection,
  organization_server_upgrades: serverUpgradeSettingsProjection,
  organization_support_access: supportAccessProjection,
} satisfies Record<CollectionName, { sources: ChangeSources; project: Projection<object> }>;
