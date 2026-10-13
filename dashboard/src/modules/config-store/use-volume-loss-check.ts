import type { DeletionCheck } from "#/components/deletion-dialog";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { StoreRefused } from "./store.contract";
import { volumeLossItems } from "#/modules/volume-placement/volume-placement";
import { volumeLoss, type VolumeLoss } from "./store-volumes";

/** What the user accepts when they confirm a Volume loss: exactly the Volumes asked about, bound to the Store's version. */
export type VolumeAcceptance = Pick<VolumeLoss, "accept" | "version">;

/** A Volume loss as the dialog asks it, and whether any of it is a restore dropping writes rather than copies going. */
export type VolumeLossCheck = DeletionCheck<VolumeAcceptance> & { losesWrites: boolean };

/**
 * Reads a write's refusal as the deletion dialog asks it: for `confirmation_required`, each Volume whose data
 * goes, with the Servers holding it by name, and what to send back to accept exactly that; null for anything else.
 */
export function useVolumeLossCheck(organizationSlug: string) {
  const { machines } = useRuntimeLens(organizationSlug);
  return (refusal: StoreRefused): VolumeLossCheck | null => {
    const refused = volumeLoss(refusal);
    if (!refused) return null;
    const items = volumeLossItems(refused.volumes, (id) => machines.find((machine) => machine.id === id)?.name);
    return { items, evidence: { accept: refused.accept, version: refused.version }, losesWrites: refused.volumes.some((volume) => volume.loss === "writes") };
  };
}
