import { useRef, useState } from "react";
import type { DiscardTarget, EnvironmentRef, RowId } from "@ployz/sdk";
import { DeletionDialog } from "#/components/deletion-dialog";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useVolumeLossCheck, type VolumeLossCheck } from "#/modules/config-store/use-volume-loss-check";
import type { PlacementLoss } from "#/modules/volume-placement/volume-placement";
type Action = "deploy" | "publish";
const NO_PLACEMENT_LOSS: PlacementLoss = { items: [], losesWrites: false, accept: [], version: (version) => version };
/** A Deploy's own words: what it ships (optional; the Store trims it). */
type Message = string | null;
/** What a Deploy or Save would delete, waiting on the user. */
type Loss = { action: Action; check: VolumeLossCheck; message: Message };
/** How a Deploy or Save went: done, failed (the writer toasted why), or asking first. */
type Outcome = { status: "done" } | { status: "failed" } | { status: "asks"; loss: Loss };

/**
 * The bottom bar's actions over the Config Store: Deploy, Save without deploying (publish), Discard and Never sync,
 * each in the Environment's write queue after its pending edits, so the CLI and this tab share one queue of Deployments.
 *
 * When a Deploy or Publish would delete Volume data the Servers hold, the Store refuses with `confirmation_required`;
 * the user reads every Volume that goes, types where, and it runs again accepting exactly those. A Deploy whose staged
 * copy changes delete a copy or drop a restore's writes asks the same way before it goes, from `placementLoss` when this
 * tab has it, else from Cloud's refusal, which checks the copies again at admission. Any other refusal (Servers that
 * can't be checked, a newer version) is the writer's toast. An admitted Deploy opens its page.
 */
export function useStoreChangeActions(organizationSlug: string, environment: EnvironmentRef,
  /** The version of the review the user sees: every action acts on exactly it, and a newer one is refused. */
  version: string, onAdmitted: (deploymentId: string) => void, placementLoss: PlacementLoss = NO_PLACEMENT_LOSS) {
  const writer = useStoreWriter(organizationSlug);
  const lossOf = useVolumeLossCheck(organizationSlug);
  // What the user types to confirm: where the data goes from.
  const place = `${environment.project ?? ""}/${environment.environment ?? ""}`;
  const [loss, setLoss] = useState<Loss | null>(null);
  // Settles the Save waiting on the dialog: true once it saves, false when the dialog closes first.
  const savePending = useRef<((saved: boolean) => void) | null>(null);
  // One admission at a time: a double click must not admit two Deployments.
  const [admitting, setAdmitting] = useState(false);
  const inFlight = useRef(false);

  /** Deploys or publishes; resolves with how it went, holding what it would delete when the Store asks first. */
  async function run(action: Action, { accept, version }: { accept: readonly string[]; version: string }, message: Message = null): Promise<Outcome> {
    const id = crypto.randomUUID();
    const words = message?.trim() || null;
    try {
      await writer.commit(action === "deploy"
        ? { command: "admit", admit: "deploy", id, environment, services: [], version, accept_volume_loss: [...accept], message: words }
        : { command: "publish", environment, version, accept_volume_loss: [...accept], message: words }, ["confirmation_required"]).isPersisted.promise;
      if (action === "deploy") onAdmitted(id);
      return { status: "done" };
    } catch (error) {
      const check = error instanceof StoreRefused ? lossOf(error) : null;
      // Anything else the writer toasted.
      return check ? { status: "asks", loss: { action, check, message } } : { status: "failed" };
    }
  }

  function ask(outcome: Outcome) {
    if (outcome.status === "asks") setLoss(outcome.loss);
    return outcome;
  }

  /** Saves without deploying; resolves true once saved, false when refused, failed or cancelled. */
  async function publish(message: Message): Promise<boolean> {
    const outcome = ask(await run("publish", { accept: [], version }, message));
    if (outcome.status !== "asks") return outcome.status === "done";
    // A later Save replaces the dialog's question: the earlier one did not save.
    settleSave(false);
    return new Promise((resolve) => { savePending.current = resolve; });
  }

  function settleSave(saved: boolean) {
    savePending.current?.(saved);
    savePending.current = null;
  }

  function deploy(message: Message) {
    if (inFlight.current) return;
    if (placementLoss.items.length > 0) {
      setLoss({ action: "deploy", check: { items: placementLoss.items, evidence: { accept: placementLoss.accept, version: placementLoss.version(version) }, losesWrites: placementLoss.losesWrites }, message });
      return;
    }
    inFlight.current = true;
    setAdmitting(true);
    void run("deploy", { accept: [], version }, message).then(ask).finally(() => {
      inFlight.current = false;
      setAdmitting(false);
    });
  }

  function discard(path: string | null, target: DiscardTarget = "head") {
    return writer.commit({ command: "discard", environment, path, version, target }).isPersisted.promise;
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
    publish,
    discard,
    neverSync,
    dialog: (
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => {
          if (open) return;
          setLoss(null);
          settleSave(false);
        }}
        title={loss?.action === "publish" ? "Saving deletes data on the next deploy" : loss?.check.losesWrites ? "Data loss" : "Deletes data"}
        place={place}
        confirmLabel={loss?.action === "publish" ? "Save" : "Deploy"}
        items={loss?.check.items}
        callbacks={{
          load: () => Promise.resolve(loss?.check ?? { items: [], evidence: { accept: [], version: "" }, losesWrites: false }),
          // Servers holding more by now: the Store asks again, and so does the dialog.
          confirm: async (evidence) => {
            if (!loss) return undefined;
            const outcome = await run(loss.action, evidence, loss.message);
            if (outcome.status === "asks") return outcome.loss.check;
            if (outcome.status === "done") settleSave(true);
            return undefined;
          },
        }}
      />
    ),
  };
}
