import type { DeletionCheck, DeletionItem } from "#/components/deletion-dialog";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { StoreRefused } from "./store.contract";
import { volumeLoss, type VolumeLoss } from "./store-volumes";

/** What the user accepts when they confirm a Volume loss: exactly the Volumes asked about, bound to the Store's version. */
export type VolumeAcceptance = Pick<VolumeLoss, "accept" | "version">;

/**
 * Reads a write's refusal as the deletion dialog asks it: for `confirmation_required`, each Volume whose data
 * goes, with the Servers holding it by name, and what to send back to accept exactly that; null for anything else.
 */
export function useVolumeLossCheck(organizationSlug: string) {
  const { machines } = useRuntimeLens(organizationSlug);
  return (refusal: StoreRefused): DeletionCheck<VolumeAcceptance> | null => {
    const refused = volumeLoss(refusal);
    if (!refused) return null;
    const items = refused.volumes.map((volume): DeletionItem => ({
      kind: "volume",
      name: volume.name,
      detail: volume.deletes.flatMap((held) => machines.find((machine) => machine.id === held.machine_id)?.name ?? []).join(", ") || undefined,
    }));
    return { items, evidence: { accept: refused.accept, version: refused.version } };
  };
}
