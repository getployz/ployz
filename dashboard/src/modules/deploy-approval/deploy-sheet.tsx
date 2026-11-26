import { useRef, useState } from "react";
import type { VersionsDraft } from "@ployz/sdk";
import { KeyRoundIcon } from "lucide-react";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { StoreRefused } from "#/lib/refusal";
import { environmentKey } from "#/modules/config-store/environment-ref";
import { invalidateViews } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { PasskeyDeclined } from "#/modules/passkeys/passkey-statement";
import { useSealing } from "#/modules/sealing/use-sealing";
import { prepareDeployTapServerFn } from "./deploy-tap.functions";
import { fillPending } from "./fill-pending";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { type Answer, type Answered, type Ask, deployWith, type DeploySteps, type Held, prepareDeploy, signVersions, withAnswer } from "./deploy-tap";
import { RuleSummaryList } from "./rule-summary";
import { ownerLookup } from "./source-step";

/** A Deploy that was not sent: refused before any prompt, stale, or cancelled. Whoever said why already showed it. */
class DeployNotSent extends Error {}

/**
 * The sheet: what it asks, the caller's own Deploy and refusals, how the caller hears back (null once admitted), and
 * what the passkey already signed for this ask, which a resend after Cloud didn't answer reuses.
 */
type Open = {
  ask: Ask; handles: readonly string[]; settle: (failed: Error | null) => void;
  problem: string | null; again: boolean; stuck?: boolean; answer?: Answer;
};

/** Tells the person their Deploy went without the automatic deploy rule, and why. */
export function warnHeld(held: Held | null) {
  if (held === null) return;
  toast.warning(held.unchecked ? "Deployed. Pushes may wait for approval." : "Deployed. Pushes wait for approval.", { description: held.reason });
}

/**
 * Admits a person's Deploy from this tab. When its Servers run a Service only signed, the sheet shows what the person's
 * passkey signs and one tap admits it. When the Store asks the caller to confirm a Volume loss, the caller's accepted
 * retry of the same plan takes the same answer with no second tap. `drop` forgets that answer when the caller cancels.
 */
export function useDeployTap(organizationSlug: string) {
  const writer = useStoreWriter(organizationSlug);
  const scope = useCollectionScope();
  const ownerOf = ownerLookup(scope.queryClient, organizationSlug);
  const [open, setOpen] = useState<Open | null>(null);
  const [busy, setBusy] = useState<"passkey" | "deploying" | null>(null);
  // The answer the caller's pending Volume-loss confirm holds, used once by its retry.
  const pending = useRef<Answered | null>(null);

  const sealing = useSealing(organizationSlug);
  const read = ({ environment, services }: AdmitDeploy) =>
    prepareDeployTapServerFn({ data: { organizationSlug, environment, services: [...services ?? []], rule: true } });
  /** The plan, once this browser fills each Pending value the Store names. */
  const prepare = async (command: AdmitDeploy) => {
    const planned = await read(command);
    const filled = await fillPending(command, planned, sealing.sealer, async (changes) => {
      const written = await writer.edit({ environment: command.environment, changes });
      return written.written === "edited" ? written.environment.revision : null;
    });
    return filled === null ? prepareDeploy(command, async () => planned, ownerOf) : prepareDeploy(filled, read, ownerOf);
  };

  async function stale(command: AdmitDeploy) {
    toast.error("Your changes moved since you looked. Check them, then deploy again.");
    await invalidateViews(scope.queryClient, organizationSlug, new Set([environmentKey(command.environment)]));
  }

  const steps: DeploySteps = {
    answered: pending,
    commit: (command, handles) => writer.commit(command, handles).then(() => undefined),
    prepare: async (command) => {
      try {
        return await prepare(command);
      } catch (error) {
        toast.error(error instanceof Error && error.message ? error.message : "Couldn't plan this deploy. Try again.");
        throw new DeployNotSent();
      }
    },
    ask: async (ask, handles) => {
      const failed = await new Promise<Error | null>((settle) => setOpen({ ask, handles, settle, problem: null, again: false }));
      if (failed !== null) throw failed;
    },
    stale: async (command) => {
      await stale(command);
      return new DeployNotSent();
    },
    refused: (message) => {
      toast.error(`${message} Nothing deployed.`);
      return new DeployNotSent();
    },
    held: warnHeld,
  };

  const admit = (command: AdmitDeploy, handles: readonly string[] = []) => deployWith(steps, command, handles);

  function close(sheet: Open, failed: Error | null) {
    sheet.settle(failed);
    setOpen(null);
  }

  async function approve(current: Open) {
    let answer = current.answer;
    try {
      if (answer === undefined) {
        setBusy("passkey");
        try {
          answer = await signVersions(current.ask);
        } catch (error) {
          const problem = error instanceof PasskeyDeclined ? error.message
            : error instanceof Error && error.name === "NotAllowedError" ? "Your passkey didn't answer. Try again."
            : "Your browser couldn't use a passkey here.";
          return setOpen({ ...current, problem });
        }
      }
      setBusy("deploying");
      await writer.commit(withAnswer(current.ask.command, answer), [...current.handles, "conflict"]);
      close(current, null);
      warnHeld(current.ask.held);
    } catch (error) {
      // The caller asks its own question (a Volume-loss confirm), and its accepted retry reuses this answer.
      if (error instanceof StoreRefused && current.handles.includes(error.code)) {
        if (answer !== undefined) {
          pending.current = { plan: current.ask.plan, place: environmentKey(current.ask.command.environment), answer, held: current.ask.held };
        }
        return close(current, error);
      }
      if (error instanceof StoreRefused) return await replan(current, error.message);
      // The Store may have admitted it: a resend carries the same Deploy and versions, so it can't admit twice.
      setOpen({ ...current, answer, problem: "Couldn't reach Ployz Cloud. Try again." });
    } finally {
      setBusy(null);
    }
  }

  /**
   * After the Store refused `current`, the plan as it is now: its lines to sign again, the Deploy itself when it no
   * longer needs a passkey, or the review when it moved. An answer never moves to another plan.
   */
  async function replan(current: Open, refusal: string) {
    // The reviewed version, so a plan that moved since takes the review path, never a direct admit.
    const prepared = await prepare(current.ask.command).catch(() => null);
    if (prepared?.kind === "ask") return setOpen({ ...current, ask: prepared.ask, answer: undefined, problem: refusal, again: true });
    if (prepared?.kind === "stale") {
      await stale(current.ask.command);
      return close(current, new DeployNotSent());
    }
    if (prepared?.kind !== "admit") return setOpen({ ...current, problem: prepared?.message ?? refusal, stuck: true });
    // A Deploy asked to sign the rule never goes on without it unseen: the person reads why and cancels.
    if (current.ask.draft.source !== undefined) return setOpen({ ...current, problem: refusal, stuck: true });
    try {
      await writer.commit(prepared.command, current.handles);
      close(current, null);
      warnHeld(prepared.held);
    } catch (error) {
      close(current, error instanceof Error ? error : new DeployNotSent());
    }
  }

  function drop() {
    pending.current = null;
  }

  const summary = open?.ask.draft.source?.summary;
  const sheet = open === null ? null : (
    <Dialog open onOpenChange={(next) => { if (!next && busy === null) close(open, new DeployNotSent()); }}>
      <DialogContent className="flex max-h-[85dvh] flex-col sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {open.ask.services.length === 0 ? `Deploy every push to ${summary?.branch ?? ""}?`
              : `Approve ${open.ask.services.join(", ")} in ${open.ask.namespace}${summary === undefined ? "" : `, and deploy every push to ${summary.branch}`}?`}
          </DialogTitle>
          <DialogDescription>
            {summary === undefined ? "Check each line. Your passkey signs exactly this." : `Check each line. Then each push to ${summary.branch} deploys these settings with no prompt.`}
          </DialogDescription>
        </DialogHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
          {summary === undefined ? null : <RuleSummaryList summary={summary} />}
          <SignedLines draft={open.ask.draft} />
        </div>
        {open.problem === null ? null : <p role="alert" className="text-sm text-destructive">{open.problem}</p>}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy !== null} />}>Cancel</DialogClose>
          <Button disabled={busy !== null || open.stuck === true} onClick={() => void approve(open)}>
            <KeyRoundIcon data-icon="inline-start" />
            {busy === "passkey" ? "Waiting for your passkey…" : busy === "deploying" ? "Deploying…" : open.answer !== undefined ? "Try again" : open.again ? "Sign and deploy again" : "Approve with passkey"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );

  return { admit, drop, sheet };
}

/** The lines a passkey signs for a Deploy, and the data it says yes to, as Rust renders them from the signed body. */
export function SignedLines({ draft }: { draft: VersionsDraft }) {
  return (
    <>
      {draft.lines.length === 0 ? null : (
        <ul aria-label="What your passkey signs" className="flex flex-col gap-1 font-mono text-xs whitespace-pre-wrap break-all">
          {draft.lines.map((line, index) => <li key={index}>{line}</li>)}
        </ul>
      )}
      {draft.confirm.length === 0 ? null : (
        <div className="flex flex-col gap-1 text-sm">
          <p className="font-medium">Needs your yes</p>
          <ul aria-label="Data your passkey says yes to" className="flex flex-col gap-1 font-mono text-xs whitespace-pre-wrap break-all">
            {draft.confirm.map((line, index) => <li key={index}>{line}</li>)}
          </ul>
          <p className="text-muted-foreground">Approving says yes to this data.</p>
        </div>
      )}
    </>
  );
}
