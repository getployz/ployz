import { useRef, useState } from "react";
import type { PostedVersion } from "@ployz/sdk";
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
import { prepareDeployTapServerFn } from "./deploy-tap.functions";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { type Ask, type Prepared, prepareDeploy, signVersions } from "./deploy-tap";

/** A Deploy that was not sent: refused before any prompt, stale, or cancelled. Whoever said why already showed it. */
class DeployNotSent extends Error {}

/**
 * The sheet: what it asks, the caller's own Deploy and refusals, how the caller hears back (null once admitted), and
 * the versions the passkey already signed for this ask, which a resend after Cloud didn't answer reuses.
 */
type Open = {
  ask: Ask; command: AdmitDeploy; handles: readonly string[]; settle: (failed: Error | null) => void;
  problem: string | null; again: boolean; stuck?: boolean; versions?: PostedVersion[];
};

/**
 * Admits a person's Deploy from this tab. When its Servers run a Service only signed, the sheet shows what the person's
 * passkey signs and one tap admits it. When the Store asks the caller to confirm a Volume loss, the caller's accepted
 * retry of the same plan takes the same answer with no second tap. `drop` forgets that answer when the caller cancels.
 */
export function useDeployTap(organizationSlug: string) {
  const writer = useStoreWriter(organizationSlug);
  const scope = useCollectionScope();
  const [open, setOpen] = useState<Open | null>(null);
  const [busy, setBusy] = useState<"passkey" | "deploying" | null>(null);
  // The answer the caller's pending Volume-loss confirm holds, used once by its retry.
  const pending = useRef<{ plan: string; place: string; versions: PostedVersion[] } | null>(null);

  const prepare = (command: AdmitDeploy) => prepareDeploy(command, ({ environment, services }) =>
    prepareDeployTapServerFn({ data: { organizationSlug, environment, services: [...services ?? []] } }));

  async function stale(command: AdmitDeploy) {
    toast.error("Your changes moved since you looked. Check them, then deploy again.");
    await invalidateViews(scope.queryClient, organizationSlug, new Set([environmentKey(command.environment)]));
  }

  async function admit(command: AdmitDeploy, handles: readonly string[] = []): Promise<void> {
    const held = pending.current;
    pending.current = null;
    let prepared: Prepared;
    try {
      prepared = await prepare(command);
    } catch (error) {
      toast.error(error instanceof Error && error.message ? error.message : "Couldn't plan this deploy. Try again.");
      throw new DeployNotSent();
    }
    if (prepared.kind === "admit") return void await writer.commit(prepared.command, handles);
    if (prepared.kind === "stale") {
      await stale(command);
      throw new DeployNotSent();
    }
    if (prepared.kind === "refused") {
      toast.error(`${prepared.message} Nothing deployed.`);
      throw new DeployNotSent();
    }
    const { ask } = prepared;
    if (held?.plan === ask.plan && held.place === environmentKey(command.environment)) {
      try {
        return void await writer.commit({ ...ask.command, versions: held.versions }, handles);
      } catch (error) {
        // The Store asks again (Servers hold more by now): the next accepted retry still needs no tap.
        if (error instanceof StoreRefused && handles.includes(error.code)) pending.current = held;
        throw error;
      }
    }
    const failed = await new Promise<Error | null>((settle) => setOpen({ ask, command, handles, settle, problem: null, again: false }));
    if (failed !== null) throw failed;
  }

  function close(sheet: Open, failed: Error | null) {
    sheet.settle(failed);
    setOpen(null);
  }

  async function approve(current: Open) {
    let versions = current.versions;
    try {
      if (versions === undefined) {
        setBusy("passkey");
        try {
          versions = await signVersions(current.ask);
        } catch (error) {
          const problem = error instanceof PasskeyDeclined ? error.message
            : error instanceof Error && error.name === "NotAllowedError" ? "Your passkey didn't answer. Try again."
            : "Your browser couldn't use a passkey here.";
          return setOpen({ ...current, problem });
        }
      }
      setBusy("deploying");
      await writer.commit({ ...current.ask.command, versions }, [...current.handles, "conflict"]);
      close(current, null);
    } catch (error) {
      // The caller asks its own question (a Volume-loss confirm), and its accepted retry reuses this answer.
      if (error instanceof StoreRefused && current.handles.includes(error.code)) {
        if (versions !== undefined) pending.current = { plan: current.ask.plan, place: environmentKey(current.command.environment), versions };
        return close(current, error);
      }
      if (error instanceof StoreRefused) return await replan(current, error.message);
      // The Store may have admitted it: a resend carries the same Deploy and versions, so it can't admit twice.
      setOpen({ ...current, versions, problem: "Couldn't reach Ployz Cloud. Try again." });
    } finally {
      setBusy(null);
    }
  }

  /**
   * After the Store refused `current`, the plan as it is now: its lines to sign again, the Deploy itself when it no
   * longer needs a passkey, or the review when it moved. An answer never moves to another plan.
   */
  async function replan(current: Open, refusal: string) {
    const prepared = await prepare(current.command).catch(() => null);
    if (prepared?.kind === "ask") return setOpen({ ...current, ask: prepared.ask, versions: undefined, problem: refusal, again: true });
    if (prepared?.kind === "stale") {
      await stale(current.command);
      return close(current, new DeployNotSent());
    }
    if (prepared?.kind !== "admit") return setOpen({ ...current, problem: prepared?.message ?? refusal, stuck: true });
    try {
      await writer.commit(prepared.command, current.handles);
      close(current, null);
    } catch (error) {
      close(current, error instanceof Error ? error : new DeployNotSent());
    }
  }

  function drop() {
    pending.current = null;
  }

  const sheet = open === null ? null : (
    <Dialog open onOpenChange={(next) => { if (!next && busy === null) close(open, new DeployNotSent()); }}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Approve {open.ask.services.join(", ")} in {open.ask.namespace}?</DialogTitle>
          <DialogDescription>Check each line. Your passkey signs exactly this.</DialogDescription>
        </DialogHeader>
        <ul aria-label="What your passkey signs" className="flex max-h-80 flex-col gap-1 overflow-auto font-mono text-xs whitespace-pre-wrap break-all">
          {open.ask.draft.lines.map((line, index) => <li key={index}>{line}</li>)}
        </ul>
        {open.problem === null ? null : <p role="alert" className="text-sm text-destructive">{open.problem}</p>}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy !== null} />}>Cancel</DialogClose>
          <Button disabled={busy !== null || open.stuck === true} onClick={() => void approve(open)}>
            <KeyRoundIcon data-icon="inline-start" />
            {busy === "passkey" ? "Waiting for your passkey…" : busy === "deploying" ? "Deploying…" : open.versions !== undefined ? "Try again" : open.again ? "Sign and deploy again" : "Approve with passkey"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );

  return { admit, drop, sheet };
}
