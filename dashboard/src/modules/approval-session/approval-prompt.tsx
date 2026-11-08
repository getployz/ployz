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
  type PromptReason,
  promptReason,
  readSession,
  SessionRefused,
  startSession,
  subscribe,
} from "./approval-session";

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

/** Resolves true when a live session covers the press, at once or after one prompt; false when the person cancels. */
export function ensureApproval(scope: ApprovalScope, family: ActionFamily): Promise<boolean> {
  const reason = promptReason(readSession(), scope, family);
  if (reason === null) return Promise.resolve(true);
  pendingAsk?.resolve(false);
  return new Promise((resolve) => setAsk({ scope, reason, resolve }));
}

/** A restart, stop or start as the browser approves it: where it runs, the Service's copies, and the Servers. */
export type ActTarget = {
  readonly environment: { readonly environmentId: string; readonly environmentName: string; readonly namespace: string };
  readonly runtime: string;
  readonly copies: readonly { readonly id: string; readonly machineId: string }[];
  readonly machines: Parameters<typeof actScope>[2];
  readonly environmentMachines: readonly string[];
};

/**
 * One approval of `verb` per copy on Servers that enforce, after one prompt when the session doesn't cover it.
 * Undefined when no Server asks for one; null when the person cancels.
 */
export async function actApprovals(verb: ServiceVerb, target: ActTarget) {
  const { enforced, scope } = actScope(target.environment, target.copies, target.machines, target.environmentMachines);
  if (!enforced || scope === null) return undefined;
  if (!(await ensureApproval(scope, verb))) return null;
  return approveAct(verb, scope, target.runtime, target.copies.map((copy) => ({ machine: copy.machineId, container: copy.id })));
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
