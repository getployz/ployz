/**
 * The one passkey prompt, mounted once per Organization page. `ensureApproval` resolves true when the session covers
 * the press, at once or after one prompt.
 */
import { useState, useSyncExternalStore } from "react";
import type { ActionFamily, ServiceVerb } from "@ployz/sdk";
import { KeyRoundIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";
import {
  actScope,
  type ApprovalScope,
  type ApprovalSession,
  approveAct,
  approveProposal,
  deployScope,
  type PromptReason,
  type ProposalStepTarget,
  promptReason,
  readSession,
  SessionRefused,
  startSession,
  subscribe,
} from "./approval-session";
import { strandedByProposal } from "#/modules/proposals/proposal";
import { needsNewerPloyz, strandedByStop } from "#/modules/service-act/service-act";

type Ask = { readonly scope: ApprovalScope; readonly reason: PromptReason; readonly resolve: (approved: boolean) => void };

let pendingAsk: Ask | null = null;
const askListeners = new Set<() => void>();
const setAsk = (ask: Ask | null) => {
  pendingAsk = ask;
  askListeners.forEach((listener) => listener());
};
const subscribeAsk = (listener: () => void) => {
  askListeners.add(listener);
  return () => void askListeners.delete(listener);
};

export const useApprovalSession = (): ApprovalSession | null => useSyncExternalStore(subscribe, readSession, () => null);

/**
 * Resolves true when a live session covers the press, at once or after one prompt; false when the person cancels. A
 * `proposal` prompts every time: the one press that signs a whole plan is never borrowed from an earlier session.
 */
export function ensureApproval(scope: ApprovalScope, family: ActionFamily | "proposal"): Promise<boolean> {
  const reason = family === "proposal" ? "proposal" : promptReason(readSession(), scope, family);
  if (reason === null) return Promise.resolve(true);
  pendingAsk?.resolve(false);
  return new Promise((resolve) => setAsk({ scope, reason, resolve }));
}

/** A restart, stop or start as the browser approves it: where it runs, the Service's copies, and the Servers. */
export type ActTarget = {
  readonly environment: { readonly environmentId: string; readonly environmentName: string; readonly namespace: string };
  readonly runtime: string;
  readonly copies: readonly { readonly id: string; readonly machineId: string; readonly hook: boolean }[];
  readonly machines: readonly (Parameters<typeof actScope>[2][number] & Parameters<typeof strandedByStop>[1][number])[];
};

/**
 * One approval of `verb` per copy on Servers that enforce, after one prompt when the session doesn't cover it.
 * Undefined when no Server asks for one; null when the person cancels. Throws, before any prompt, for a stop that a
 * Server couldn't later start again.
 */
export async function actApprovals(verb: ServiceVerb, target: ActTarget) {
  const stranded = verb === "stop" ? strandedByStop(target.copies, target.machines) : null;
  if (stranded !== null) throw new Error(needsNewerPloyz(stranded, "stop"));
  const { enforced, scope } = actScope(target.environment, target.copies, target.machines);
  if (!enforced || scope === null) return undefined;
  if (!(await ensureApproval(scope, verb))) return null;
  return approveAct(verb, scope, target.runtime, target.copies.map((copy) => ({ machine: copy.machineId, container: copy.id, hook: copy.hook })));
}

/** A proposal as the browser approves it: each step's copies, and the Servers, in one Environment. */
export type ProposalTarget = {
  readonly environment: ActTarget["environment"];
  readonly steps: readonly { readonly verb: ServiceVerb; readonly runtime: string; readonly copies: ActTarget["copies"] }[];
  readonly machines: ActTarget["machines"];
};

/**
 * One bundle over every step, after exactly one prompt. Null when the person cancels. Throws, before any prompt, for a
 * step a Server couldn't take: one that hasn't learnt proposals, or a stop it couldn't start again.
 */
export async function proposalApprovals(target: ProposalTarget) {
  for (const step of target.steps) {
    const older = strandedByProposal(step.copies, target.machines);
    if (older !== null) throw new Error(needsNewerPloyz(older, step.verb));
    const stranded = step.verb === "stop" ? strandedByStop(step.copies, target.machines) : null;
    if (stranded !== null) throw new Error(needsNewerPloyz(stranded, "stop"));
  }
  const copies = target.steps.flatMap((step) => step.copies);
  const { scope } = actScope(target.environment, copies, target.machines);
  if (scope === null) throw new Error("Cloud couldn't check the servers. Try again.");
  if (!(await ensureApproval(scope, "proposal"))) return null;
  const steps: ProposalStepTarget[] = target.steps.map((step) => ({
    verb: step.verb,
    runtime: step.runtime,
    copies: step.copies.map((copy) => ({ machine: copy.machineId, container: copy.id, hook: copy.hook })),
  }));
  return approveProposal(steps, scope);
}

/** A Deploy as the browser approves it: the Environment, the Servers running it, and every Server. */
export type DeployTarget = {
  readonly environment: ActTarget["environment"];
  readonly environmentMachines: readonly string[];
  readonly machines: Parameters<typeof deployScope>[2];
};

/**
 * The scope a Deploy's approval names, after one prompt when the session doesn't cover it. Undefined when no Server
 * enforces, so the Deploy goes as before; null when the person cancels. Throws, before any prompt, when a Server that
 * enforces can't take an approval from a browser, or Cloud can't tell which Cluster the Deploy goes to: either way the
 * Deploy would only be refused there.
 */
export async function deployApprovalScope(target: DeployTarget): Promise<ApprovalScope | null | undefined> {
  const { enforced, scope, older } = deployScope(target.environment, target.environmentMachines, target.machines);
  if (!enforced) return undefined;
  if (older !== null) throw new Error(`${older} needs a newer Ployz to deploy from the dashboard. Upgrade it, or run ployz deploy.`);
  if (scope === null) throw new Error("Cloud couldn't check the servers. Try again.");
  return (await ensureApproval(scope, "deploy")) ? scope : null;
}

export function ApprovalPrompt() {
  const ask = useSyncExternalStore(subscribeAsk, () => pendingAsk, () => null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  if (ask === null) return null;
  const close = (approved: boolean) => {
    setFailed(null);
    ask.resolve(approved);
    setAsk(null);
  };
  const confirm = async () => {
    if (busy) return;
    setBusy(true);
    setFailed(null);
    try {
      await startSession(ask.scope);
      close(true);
    } catch (error) {
      setFailed(error instanceof SessionRefused ? error.message
        : error instanceof Error && error.name === "NotAllowedError" ? "Your passkey didn't answer. Try again."
        : "Your browser couldn't use a passkey here.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !busy) close(false); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Confirm it's you</DialogTitle>
          <DialogDescription>Use your passkey to continue.</DialogDescription>
        </DialogHeader>
        {failed !== null && <p role="alert" className="text-sm text-destructive">{failed}</p>}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy} />}>Cancel</DialogClose>
          <Button disabled={busy} onClick={() => void confirm()}>
            {busy ? <Spinner data-icon="inline-start" /> : <KeyRoundIcon data-icon="inline-start" />}
            Use passkey
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
