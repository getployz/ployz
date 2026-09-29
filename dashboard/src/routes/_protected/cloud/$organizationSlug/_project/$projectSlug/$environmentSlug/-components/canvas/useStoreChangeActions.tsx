import { useState } from "react";
import type { EnvironmentRef } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { useStoreWriter, StoreRefused } from "#/modules/config-store/store-write";
import { volumeLoss, type VolumeLoss } from "#/modules/config-store/store-volumes";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";

type Acceptance = Pick<VolumeLoss, "accept" | "version">;
type Action = "deploy" | "publish";

/**
 * The bottom bar's actions over the Config Store: Deploy, Save without deploying (publish) and Discard, each in the
 * Environment's write queue after its pending edits, so the CLI and this tab share one queue of Deployments.
 *
 * When a Deploy or Publish would delete Volume data the Servers hold, the Store refuses with `confirmation_required`;
 * the user reads every Volume that goes, types where, and it runs again accepting exactly those. Any other refusal
 * (Servers that can't be checked, a newer version) is the writer's toast. An admitted Deploy opens its page.
 */
export function useStoreChangeActions(organizationSlug: string, environment: EnvironmentRef,
  /** The version of the review the user sees: every action acts on exactly it, and a newer one is refused. */
  version: string, onAdmitted: (deploymentId: string) => void) {
  const writer = useStoreWriter(organizationSlug);
  const { machines } = useRuntimeLens(organizationSlug);
  // What the user types to confirm: where the data goes from.
  const place = `${environment.project ?? ""}/${environment.environment ?? ""}`;
  const [loss, setLoss] = useState<{ action: Action; check: DeletionCheck<Acceptance> } | null>(null);

  function check(refused: VolumeLoss): DeletionCheck<Acceptance> {
    const items = refused.volumes.map((volume): DeletionItem => ({
      kind: "volume",
      name: volume.name,
      // The Servers holding its data, by name.
      detail: volume.deletes.flatMap((held) => machines.find((machine) => machine.id === held.machine_id)?.name ?? []).join(", ") || undefined,
    }));
    return { items, evidence: { accept: refused.accept, version: refused.version } };
  }

  /** Deploys or publishes; resolves with what it would delete when the Store asks first, else null. */
  async function run(action: Action, { accept, version }: { accept: readonly string[]; version: string }) {
    const id = crypto.randomUUID();
    try {
      await writer.commit(action === "deploy"
        ? { command: "admit", id, environment, services: [], version, remove: false, accept_volume_loss: [...accept] }
        : { command: "publish", environment, version, accept_volume_loss: [...accept] }).isPersisted.promise;
      if (action === "deploy") onAdmitted(id);
      return null;
    } catch (error) {
      const refused = error instanceof StoreRefused ? volumeLoss(error) : null;
      if (refused) return { action, check: check(refused) };
      // The writer toasted it.
      return null;
    }
  }

  /** Discards `path` (`SERVICE` or `SERVICE.SETTING`; null for everything); resolves whether it did. */
  async function discard(path: string | null) {
    try {
      await writer.commit({ command: "discard", environment, path, version }).isPersisted.promise;
      return true;
    } catch {
      // The writer toasted it and refetched the review.
      return false;
    }
  }

  return {
    deploy: () => void run("deploy", { accept: [], version }).then(setLoss),
    publish: () => void run("publish", { accept: [], version }).then(setLoss),
    discard,
    dialog: (
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => { if (!open) setLoss(null); }}
        title={loss?.action === "publish" ? "Publishing deletes data on the next deploy" : "Deploy deletes data"}
        place={place}
        confirmLabel={loss?.action === "publish" ? "Publish" : "Deploy"}
        items={loss?.check.items}
        callbacks={{
          load: () => Promise.resolve(loss?.check ?? { items: [], evidence: { accept: [], version: "" } }),
          // Servers holding more by now: the Store asks again, and so does the dialog.
          confirm: async (evidence) => loss ? (await run(loss.action, evidence))?.check : undefined,
        }}
      />
    ),
  };
}
