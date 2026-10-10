import { useRef, useState } from "react";
import type { DiscardTarget, EnvironmentRef, RowId } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck } from "#/components/deletion-dialog";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useVolumeLossCheck, type VolumeAcceptance as Acceptance } from "#/modules/config-store/use-volume-loss-check";
type Action = "deploy" | "publish";
/** A Deploy's own words: what it ships (optional; the Store trims it). */
type Message = string | null;

/**
 * The bottom bar's actions over the Config Store: Deploy, Save without deploying (publish), Discard and Never sync,
 * each in the Environment's write queue after its pending edits, so the CLI and this tab share one queue of Deployments.
 *
 * When a Deploy or Publish would delete Volume data the Servers hold, the Store refuses with `confirmation_required`;
 * the user reads every Volume that goes, types where, and it runs again accepting exactly those. Any other refusal
 * (Servers that can't be checked, a newer version) is the writer's toast. An admitted Deploy opens its page.
 */
export function useStoreChangeActions(organizationSlug: string, environment: EnvironmentRef,
  /** The version of the review the user sees: every action acts on exactly it, and a newer one is refused. */
  version: string, onAdmitted: (deploymentId: string) => void) {
  const writer = useStoreWriter(organizationSlug);
  const lossOf = useVolumeLossCheck(organizationSlug);
  // What the user types to confirm: where the data goes from.
  const place = `${environment.project ?? ""}/${environment.environment ?? ""}`;
  const [loss, setLoss] = useState<{ action: Action; check: DeletionCheck<Acceptance>; message: Message } | null>(null);
  // One admission at a time: a double click must not admit two Deployments.
  const [admitting, setAdmitting] = useState(false);
  const inFlight = useRef(false);

  /** Deploys or publishes; resolves with what it would delete when the Store asks first, else null. */
  async function run(action: Action, { accept, version }: { accept: readonly string[]; version: string }, message: Message = null) {
    const id = crypto.randomUUID();
    const words = message?.trim() || null;
    try {
      await writer.commit(action === "deploy"
        ? { command: "admit", admit: "deploy", id, environment, services: [], version, accept_volume_loss: [...accept], message: words }
        : { command: "publish", environment, version, accept_volume_loss: [...accept], message: words }, ["confirmation_required"]).isPersisted.promise;
      if (action === "deploy") onAdmitted(id);
      return null;
    } catch (error) {
      const check = error instanceof StoreRefused ? lossOf(error) : null;
      // Anything else the writer toasted.
      return check && { action, check, message };
    }
  }

  function deploy(message: Message) {
    if (inFlight.current) return;
    inFlight.current = true;
    setAdmitting(true);
    void run("deploy", { accept: [], version }, message).then(setLoss).finally(() => {
      inFlight.current = false;
      setAdmitting(false);
    });
  }

  function discard(path: string | null, target: DiscardTarget = "head") {
    writer.commit({ command: "discard", environment, path, version, target });
  }

  /** Marks `row` Never sync here and discards `path`, all or none: what arrived goes, and nothing follows into it again. */
  function neverSync(path: string, row: RowId) {
    writer.commit({ command: "batch", environment, commands: [
      { command: "never_sync", environment, rows: [row] },
      { command: "discard", environment, path, version },
    ] });
  }

  return {
    deploy,
    admitting,
    publish: (message: Message) => void run("publish", { accept: [], version }, message).then(setLoss),
    discard,
    neverSync,
    dialog: (
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => { if (!open) setLoss(null); }}
        title={loss?.action === "publish" ? "Saving deletes data on the next deploy" : "Deploy deletes data"}
        place={place}
        confirmLabel={loss?.action === "publish" ? "Save" : "Deploy"}
        items={loss?.check.items}
        callbacks={{
          load: () => Promise.resolve(loss?.check ?? { items: [], evidence: { accept: [], version: "" } }),
          // Servers holding more by now: the Store asks again, and so does the dialog.
          confirm: async (evidence) => loss ? (await run(loss.action, evidence, loss.message))?.check : undefined,
        }}
      />
    ),
  };
}
