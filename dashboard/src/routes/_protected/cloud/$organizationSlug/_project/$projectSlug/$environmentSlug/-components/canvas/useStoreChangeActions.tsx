import { useState } from "react";
import type { EnvironmentRef } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { useStoreWriter, StoreRefused } from "#/modules/config-store/store-write";
import { volumeLoss, type VolumeLoss } from "#/modules/config-store/store-volumes";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";

type Acceptance = Pick<VolumeLoss, "accept" | "version">;

/**
 * The bottom bar's actions over the Config Store: Deploy, Save without deploying (publish) and Discard, each in the
 * Environment's write queue after its pending edits, so the CLI and this tab share one queue of Deployments.
 *
 * When a Deploy would delete Volume data the Servers hold, the Store refuses with `confirmation_required`; the user
 * reads every Volume that goes, types where, and the Deploy is admitted again accepting exactly those. Any other
 * refusal (Servers that can't be checked, a newer version) is the writer's toast. An admitted Deploy opens its page.
 */
export function useStoreChangeActions(organizationSlug: string, environment: EnvironmentRef, environmentId: string,
  onAdmitted: (deploymentId: string) => void) {
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
    const id = crypto.randomUUID();
    try {
      await writer.commit({ command: "admit", id, environment, services: [], version, accept_volume_loss: [...accept] }).isPersisted.promise;
      onAdmitted(id);
      return null;
    } catch (error) {
      const refused = error instanceof StoreRefused ? volumeLoss(error) : null;
      if (refused) return check(refused);
      // The writer toasted it.
      return null;
    }
  }

  /** Discards `path` (`SERVICE` or `SERVICE.SETTING`; null for everything); resolves whether it did. */
  async function discard(path: string | null) {
    try {
      await writer.commit({ command: "discard", environment, path, version: null }).isPersisted.promise;
      return true;
    } catch {
      // The writer toasted it and refetched the review.
      return false;
    }
  }

  return {
    deploy: () => void admit({ accept: [], version: null }).then(setLoss),
    publish: () => { writer.commit({ command: "publish", environment, version: null }); },
    discard,
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
