import { canvasPositions } from "#/modules/canvas/canvas.collection";
import { clusterDomain } from "#/modules/cluster-domain/cluster-domain.collection";
import { clusterName } from "#/modules/cluster-name/cluster-name.collection";
import { clusterIngressChange } from "#/modules/ingress/doors.queries";
import { organizationEnrollment } from "#/modules/machines/enrollment.collection";
import { serverDrainChange } from "#/modules/machines/server-drain.queries";
import { serverWebTrafficChange } from "#/modules/machines/server-web-traffic.queries";
import { membersChange } from "#/modules/organization/members.queries";
import { organizationChange } from "#/modules/organization/organization-state.queries";
import { serverUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.collection";
import { serverUpgradeChange } from "#/modules/server-upgrade/server-upgrade.queries";
import { serviceActChange } from "#/modules/service-act/service-act.queries";
import { supportAccess } from "#/modules/support-access/support-access.collection";
import { volumePlacementChange } from "#/modules/volume-placement/volume-placement.queries";
import { volumeRunChange } from "#/modules/volume-run/volume-run.queries";

/**
 * Every Org Store collection. Adding one is a declaration in its feature's `*.collection.ts`, a projection in its
 * `*.collection.server.ts` (listed in `registry.server.ts`), and a line here.
 */
export const orgCollections = [canvasPositions, organizationEnrollment, clusterDomain, serverUpgradeSettings, supportAccess, clusterName] as const;

/** Every other change name a feature owns. The Config Store's families are `storeViewNames`. */
export const orgChanges = [
  organizationChange, serverUpgradeChange, serverDrainChange, serverWebTrafficChange, clusterIngressChange,
  volumeRunChange, serviceActChange, volumePlacementChange, membersChange,
] as const;

type NamesOf<T extends ReadonlyArray<{ readonly name: string }>> = { readonly [K in keyof T]: T[K]["name"] };

/** The names of `declarations`, keeping each literal. */
export const namesOf = <const T extends ReadonlyArray<{ readonly name: string }>>(declarations: T): NamesOf<T> => {
  const names = declarations.map((declaration) => declaration.name);
  // SAFETY: `NamesOf` maps each element to its own `name`, which is what `map` returns in the same order.
  return names as NamesOf<T>;
};
