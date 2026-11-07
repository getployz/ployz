/**
 * The one passkey prompt, mounted once per Organization page, and the badge that says how long a session still covers
 * an Environment. `ensureApproval` resolves true when the session covers the press, at once or after one prompt.
 */
import { useState, useSyncExternalStore } from "react";
import type { ActionFamily } from "@ployz/sdk";
import { KeyRoundIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";
import { useMinute } from "#/hooks/use-minute";
import {
  type ApprovalScope,
  type ApprovalSession,
  minutesLeft,
  type PromptReason,
  promptReason,
  readSession,
  SESSION_MINUTES,
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

const copyOf = ({ scope, reason }: Ask) => {
  const where = scope.environmentName;
  switch (reason) {
    case "first":
      return { title: "Confirm it's you", description: `Use your passkey to approve restarts in ${where} for the next ${SESSION_MINUTES} minutes.` };
    case "expired":
      return { title: "Confirm it's you again", description: `Your approval for ${where} ended. Use your passkey to approve restarts there for the next ${SESSION_MINUTES} minutes.` };
    case "scope":
      return { title: "This needs its own approval", description: `Your approval covers ${readSession()?.scope.environmentName ?? "another environment"} only. Use your passkey to approve restarts in ${where} for the next ${SESSION_MINUTES} minutes.` };
  }
};

export function ApprovalPrompt() {
  const ask = useSyncExternalStore(subscribeAsk, () => pendingAsk, () => null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  if (ask === null) return null;
  const { title, description } = copyOf(ask);
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
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
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

/** How long your passkey approval still covers this Environment, or that it ended. Nothing before the first prompt. */
export function ApprovalSessionBadge({ environmentId }: { environmentId: string }) {
  const session = useApprovalSession();
  const now = useMinute();
  if (session === null || now === null) return null;
  const hint = `Approved with your passkey, key ${session.keyCode}`;
  const left = minutesLeft(session, now.getTime());
  if (left === 0) return <Badge variant="warning" title={hint}><KeyRoundIcon data-icon="inline-start" />Approval ended</Badge>;
  if (session.scope.environmentId !== environmentId) {
    return <Badge variant="outline" title={hint}><KeyRoundIcon data-icon="inline-start" />Approved for {session.scope.environmentName} only</Badge>;
  }
  return <Badge variant="outline" title={hint}><KeyRoundIcon data-icon="inline-start" />Approved for {session.scope.environmentName} · {left} min left</Badge>;
}
