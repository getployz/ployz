/**
 * A copy of a sealed value this browser made, such as a Database Preset's password, arrives without its value: Pending.
 * The Store refuses a Deploy that needs one and lists each. Before the Deploy is planned again, this browser makes each
 * value, seals it, and writes them in one edit, so Cloud never sees one.
 */
import type { Change } from "@ployz/sdk";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { envPath } from "#/modules/config-store/setting-paths";
import type { Sealer } from "#/modules/sealing/use-sealing";
import { madeSecret } from "#/modules/sealing/made-secret";
import { isPending, moved } from "./deploy-tap";
import type { DeployPlan } from "./deploy-tap.server";

/**
 * `command` once each Pending value `planned` names is made, sealed and written by `edit`, which answers the revision
 * it wrote. Its version moves past that one write, so the review it names still holds. Null when nothing is Pending.
 */
export async function fillPending(
  command: AdmitDeploy,
  planned: DeployPlan,
  seal: () => Promise<Sealer>,
  edit: (changes: Change[]) => Promise<number | null>,
): Promise<AdmitDeploy | null> {
  if (planned.kind !== "refused" || !isPending(planned.details)) return null;
  const { environment, pending } = planned.details;
  const sealer = await seal();
  const changes = await Promise.all(pending.map(async ({ service, id, key, generated }): Promise<Change> => {
    const value = await sealer({ key, value: madeSecret(generated), target: { environment, service: id } });
    return { op: "set", path: envPath(service, key), value: { secret: { ...value, generated } } };
  }));
  const written = await edit(changes);
  return { ...command, version: moved(command.version, written) };
}
