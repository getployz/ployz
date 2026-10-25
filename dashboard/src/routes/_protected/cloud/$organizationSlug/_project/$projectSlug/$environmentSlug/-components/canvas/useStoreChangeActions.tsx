import { useEffect, useRef, useState } from "react";
import type { ConfigCommand, DiscardTarget, EnvironmentRef, RowId } from "@ployz/sdk";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { DeletionDialog } from "#/components/deletion-dialog";
import { reviewMoving, useStoreBusy, writeMark } from "#/modules/config-store/store-pending";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useVolumeLossCheck, type VolumeLossCheck } from "#/modules/config-store/use-volume-loss-check";
import type { PlacementLoss } from "#/modules/volume-placement/volume-placement";
type Action = "deploy" | "publish";
const NO_PLACEMENT_LOSS: PlacementLoss = { items: [], losesWrites: false, accept: [], version: (version) => version };
/** A Deploy's own words: what it ships (optional; the Store trims it). */
type Message = string | null;
/** Whether a Deploy keeps the Services the Environment runs but doesn't hold; by default it removes them. */
export type DeployChoice = { noPrune: boolean };
const PRUNES: DeployChoice = { noPrune: false };
/** What a Deploy or Save would delete, waiting on the user. */
type Loss = { action: Action; check: VolumeLossCheck; message: Message; choice: DeployChoice };
/** How a Deploy or Save went: done, failed (the writer toasted why), asking first, or not sent while a field still stages. */
type Outcome = { status: "done" } | { status: "failed" } | { status: "asks"; loss: Loss } | { status: "staging" };

/**
 * The bottom bar's actions over the Config Store: Deploy, which removes the Services the Environment runs but doesn't
 * hold unless told to keep them, Save only (the Store's publish), Discard and Never sync,
 * each in the Environment's write queue after its pending edits, so the CLI and this tab share one queue of Deployments.
 *
 * When a Deploy or Save only would delete Volume data the Servers hold, the Store refuses with `confirmation_required`;
 * the user reads every Volume that goes, types where, and it runs again accepting exactly those. A Deploy whose staged
 * copy changes delete a copy or drop a restore's writes asks the same way before it goes, from `placementLoss` when this
 * tab has it, else from Cloud's refusal, which checks the copies again at admission. Any other refusal (Servers that
 * can't be checked, a newer version) is the writer's toast. An admitted Deploy opens its page.
 *
 * Pressing Deploy or Save in a field with a draft stages the draft first (focus leaves the field) and sends nothing,
 * even when the draft saved before the click: the review the user read as they pressed doesn't include it. Nor does
 * a press while any other write that moves the review still saves. The bar then shows the draft, pink and counted,
 * and the next press ships exactly that.
 */
export function useStoreChangeActions(organizationSlug: string, environment: EnvironmentRef,
  /** The version of the review the user sees: every action acts on exactly it, and a newer one is refused. */
  version: string, onAdmitted: (deploymentId: string) => void, placementLoss: PlacementLoss = NO_PLACEMENT_LOSS) {
  const writer = useStoreWriter(organizationSlug);
  const { queryClient } = useCollectionScope();
  const lossOf = useVolumeLossCheck(organizationSlug);
  // What the user types to confirm: where the data goes from.
  const place = `${environment.project ?? ""}/${environment.environment ?? ""}`;
  const [loss, setLoss] = useState<Loss | null>(null);
  // Settles the Save waiting on the dialog: true once it saves, false when the dialog closes first.
  const savePending = useRef<((saved: boolean) => void) | null>(null);
  const admitting = useStoreBusy(organizationSlug, environment).has("deploying");
  // The mark shows only after a render, so two clicks in one tick need this lock to admit one Deployment.
  const inFlight = useRef(false);
  // When the press began: the newest write then, and whether one moving the review was saving. A pointer press starts
  // before the field it leaves blurs, so a press ships only if no such write was in flight at any point since it began.
  const gesture = useRef({ mark: Number.POSITIVE_INFINITY, saving: false });
  useEffect(() => {
    // A held key's repeats are the same press. A held Enter's repeat clicks are deliberate presses of their own.
    const mark = (event: Event) => {
      if (event instanceof KeyboardEvent && event.repeat) return;
      gesture.current = { mark: writeMark(queryClient), saving: reviewMoving(queryClient, organizationSlug, environment, Number.POSITIVE_INFINITY) };
    };
    document.addEventListener("pointerdown", mark, true);
    document.addEventListener("keydown", mark, true);
    return () => {
      document.removeEventListener("pointerdown", mark, true);
      document.removeEventListener("keydown", mark, true);
    };
  }, [queryClient, organizationSlug, environment]);

  /** Deploys or saves; resolves with how it went, holding what it would delete when the Store asks first. */
  async function run(action: Action, { accept, version }: { accept: readonly string[]; version: string }, message: Message = null, choice = PRUNES): Promise<Outcome> {
    // Read now, not from the last render: the press that got here may have just staged the field it left.
    const { mark, saving } = gesture.current;
    // Each press is judged once: a later one with no pointer or key before it (assistive tech) starts from here.
    gesture.current = { mark: writeMark(queryClient), saving: false };
    if (saving || reviewMoving(queryClient, organizationSlug, environment, mark)) return { status: "staging" };
    const id = crypto.randomUUID();
    const words = message?.trim() || null;
    const command: ConfigCommand = action === "deploy"
      ? { command: "admit", admit: "deploy", id, environment, services: [], version, accept_volume_loss: [...accept], message: words }
      : { command: "publish", environment, version, accept_volume_loss: [...accept], message: words };
    // A Store from before no_prune refuses a Deploy carrying it, so only a Deploy that keeps other services sends it.
    if (command.command === "admit" && command.admit === "deploy" && choice.noPrune) command.no_prune = true;
    try {
      await writer.commit(command, ["confirmation_required"]).isPersisted.promise;
      if (action === "deploy") onAdmitted(id);
      return { status: "done" };
    } catch (error) {
      const check = error instanceof StoreRefused ? lossOf(error) : null;
      // Anything else the writer toasted.
      return check ? { status: "asks", loss: { action, check, message, choice } } : { status: "failed" };
    }
  }

  function ask(outcome: Outcome) {
    if (outcome.status === "asks") setLoss(outcome.loss);
    return outcome;
  }

  /** Save only: saves without deploying; resolves true once saved, false when refused, failed or cancelled. */
  async function save(message: Message): Promise<boolean> {
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

  function deploy(message: Message, choice = PRUNES) {
    if (inFlight.current) return;
    if (placementLoss.items.length > 0) {
      setLoss({ action: "deploy", check: { items: placementLoss.items, evidence: { accept: placementLoss.accept, version: placementLoss.version(version) }, losesWrites: placementLoss.losesWrites }, message, choice });
      return;
    }
    inFlight.current = true;
    void run("deploy", { accept: [], version }, message, choice).then(ask).finally(() => { inFlight.current = false; });
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
    save,
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
        title={loss?.action === "publish" ? "Next deploy deletes data" : loss?.check.losesWrites ? "Data loss" : "Deletes data"}
        place={place}
        sentence={loss?.action === "publish" ? "Saving keeps data for now. The next deploy deletes it, including one a Git push starts." : undefined}
        confirmLabel={loss?.action === "publish" ? "Save only" : "Deploy"}
        items={loss?.check.items}
        callbacks={{
          load: () => Promise.resolve(loss?.check ?? { items: [], evidence: { accept: [], version: "" }, losesWrites: false }),
          // Servers holding more by now: the Store asks again, and so does the dialog.
          confirm: async (evidence) => {
            if (!loss) return undefined;
            const outcome = await run(loss.action, evidence, loss.message, loss.choice);
            if (outcome.status === "asks") return outcome.loss.check;
            if (outcome.status === "done") settleSave(true);
            return undefined;
          },
        }}
      />
    ),
  };
}
