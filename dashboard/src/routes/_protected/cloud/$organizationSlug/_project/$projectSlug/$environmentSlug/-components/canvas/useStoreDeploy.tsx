import { useState } from "react";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { useStoreWriter, StoreRefused } from "#/modules/config-store/store-write";
import { volumeLoss, type VolumeLoss } from "#/modules/config-store/store-volumes";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";

type Acceptance = Pick<VolumeLoss, "accept" | "version">;

/**
 * Deploys an Environment's Working State through the Config Store. When the Deploy would delete Volume data the
 * Servers hold, the Store refuses with `confirmation_required`; the user reads every Volume that goes, types where,
 * and the Deploy is admitted again accepting exactly those. Any other refusal (Servers that can't be checked, a
 * newer version) is the writer's toast.
 */
// TODO(#1273): the review, Discard and the Deployment Page move here too.
export function useStoreDeploy(organizationSlug: string, environment: EnvironmentRef, environmentId: string) {
  const writer = useStoreWriter(organizationSlug);
  const { machines } = useRuntimeLens(organizationSlug);
  const place = useEnvironmentPlace(organizationSlug, environmentId);
  const [loss, setLoss] = useState<DeletionCheck<Acceptance> | null>(null);

  function check(refused: VolumeLoss): DeletionCheck<Acceptance> {
    const items = refused.volumes.map((volume): DeletionItem => ({
      kind: "volume",
      name: volume.name,
      // The Servers holding its data, by name.
      detail: volume.deletes.flatMap((held) => machines.find((machine) => machine.id === held.machine_id)?.name ?? []).join(", ") || undefined,
    }));
    return { items, evidence: { accept: refused.accept, version: refused.version } };
  }

  /** Admits the Deploy; resolves with what it would delete when the Store asks first, else null. */
  async function admit({ accept, version }: { accept: readonly string[]; version: string | null }) {
    try {
      await writer.commit({
        command: "admit", id: crypto.randomUUID(), environment, services: [], version, accept_volume_loss: [...accept],
      }).isPersisted.promise;
      toast("Deploy queued.");
      return null;
    } catch (error) {
      const refused = error instanceof StoreRefused ? volumeLoss(error) : null;
      if (refused) return check(refused);
      // The writer toasted it.
      return null;
    }
  }

  return {
    deploy: () => void admit({ accept: [], version: null }).then(setLoss),
    dialog: (
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => { if (!open) setLoss(null); }}
        title="Deploy deletes data"
        place={place}
        confirmLabel="Deploy"
        items={loss?.items}
        callbacks={{
          load: () => Promise.resolve(loss ?? { items: [], evidence: { accept: [], version: "" } }),
          // Servers holding more by now: the Store asks again, and so does the dialog.
          confirm: async (evidence) => (await admit(evidence)) ?? undefined,
        }}
      />
    ),
  };
}
