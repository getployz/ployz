import { useRef, useState } from "react";
import type { PostedVersion } from "@ployz/sdk";
import { KeyRoundIcon } from "lucide-react";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { StoreRefused } from "#/lib/refusal";
import { environmentKey } from "#/modules/config-store/environment-ref";
import { fetchStoreView, invalidateViews } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { PasskeyDeclined } from "#/modules/passkeys/passkey-statement";
import { prepareDeployTapServerFn } from "./deploy-tap.functions";
import { type AdmitDeploy, type Ask, type Prepared, prepareDeploy, signVersions } from "./deploy-tap";

/** A Deploy that was not sent: refused before any prompt, stale, or cancelled. Whoever said why already showed it. */
export class DeployNotSent extends Error {}

/** The sheet: what it asks, the caller's own Deploy and refusals, and how the caller hears back (null once admitted). */
type Open = { ask: Ask; command: AdmitDeploy; handles: readonly string[]; settle: (failed: Error | null) => void; problem: string | null; again: boolean; stuck?: boolean };

/**
 * Admits a person's Deploy from this tab. When its Servers run a Service only signed, the sheet shows what the person's
 * passkey signs and one tap admits it. A Store that asks again for the same plan, as a Volume-loss confirm does, takes
 * the same answer with no second tap.
 */
export function useDeployTap(organizationSlug: string) {
  const writer = useStoreWriter(organizationSlug);
  const scope = useCollectionScope();
  const [open, setOpen] = useState<Open | null>(null);
  const [busy, setBusy] = useState<"passkey" | "deploying" | null>(null);
  const kept = useRef<{ plan: string; place: string; versions: PostedVersion[] } | null>(null);

  const prepare = (command: AdmitDeploy) => prepareDeploy(command,
    (query) => fetchStoreView(organizationSlug, scope, query),
    (tags) => prepareDeployTapServerFn({ data: { organizationSlug, tags } }));

  async function admit(command: AdmitDeploy, handles: readonly string[] = []): Promise<void> {
    let prepared: Prepared;
    try {
      prepared = await prepare(command);
    } catch (error) {
      toast.error(error instanceof Error && error.message ? error.message : "Couldn't plan this deploy. Try again.");
      throw new DeployNotSent();
    }
    if (prepared.kind === "admit") return void await writer.commit(prepared.command, handles);
    if (prepared.kind === "stale") {
      toast.error("Your changes moved since you looked. Check them, then deploy again.");
      await invalidateViews(scope.queryClient, organizationSlug, new Set([environmentKey(command.environment)]));
      throw new DeployNotSent();
    }
    if (prepared.kind === "refused") {
      toast.error(`${prepared.message} Nothing deployed.`);
      throw new DeployNotSent();
    }
    const { ask } = prepared;
    const place = environmentKey(command.environment);
    if (kept.current?.plan === ask.plan && kept.current.place === place) {
      return void await writer.commit({ ...ask.command, versions: kept.current.versions }, handles);
    }
    const failed = await new Promise<Error | null>((settle) => setOpen({ ask, command, handles, settle, problem: null, again: false }));
    if (failed !== null) throw failed;
  }

  function close(sheet: Open, failed: Error | null) {
    sheet.settle(failed);
    setOpen(null);
  }

  async function approve(current: Open) {
    setBusy("passkey");
    try {
      const versions = await signVersions(current.ask);
      kept.current = { plan: current.ask.plan, place: environmentKey(current.command.environment), versions };
      setBusy("deploying");
      await writer.commit({ ...current.ask.command, versions }, [...current.handles, "conflict"]);
      close(current, null);
    } catch (error) {
      // The caller asks its own question (a Volume-loss confirm), and its retry reuses this answer.
      if (error instanceof StoreRefused && current.handles.includes(error.code)) return close(current, error);
      if (error instanceof StoreRefused) return await replan(current, error.message);
      const problem = error instanceof PasskeyDeclined ? error.message
        : error instanceof Error && error.name === "NotAllowedError" ? "Your passkey didn't answer. Try again."
        : "Your browser couldn't use a passkey here.";
      setOpen({ ...current, problem });
    } finally {
      setBusy(null);
    }
  }

  /** After the Store refused `current`, the sheet shows the plan as it is now. An answer never moves to another plan. */
  async function replan(current: Open, refusal: string) {
    const prepared = await prepare(current.command).catch(() => null);
    if (prepared?.kind === "ask") return setOpen({ ...current, ask: prepared.ask, problem: refusal, again: true });
    setOpen({ ...current, problem: refusal, again: false, stuck: true });
  }

  const sheet = open === null ? null : (
    <Dialog open onOpenChange={(next) => { if (!next && busy === null) close(open, new DeployNotSent()); }}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Approve {open.ask.services.join(", ")} in {open.ask.namespace}?</DialogTitle>
          <DialogDescription>Check each line. Your passkey signs exactly this.</DialogDescription>
        </DialogHeader>
        <ul aria-label="What your passkey signs" className="flex max-h-80 flex-col gap-1 overflow-auto font-mono text-xs break-all">
          {open.ask.draft.lines.map((line, index) => <li key={index}>{line}</li>)}
        </ul>
        {open.problem === null ? null : <p role="alert" className="text-sm text-destructive">{open.problem}</p>}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy !== null} />}>Cancel</DialogClose>
          <Button disabled={busy !== null || open.stuck === true} onClick={() => void approve(open)}>
            <KeyRoundIcon data-icon="inline-start" />
            {busy === "passkey" ? "Waiting for your passkey…" : busy === "deploying" ? "Deploying…" : open.again ? "Sign and deploy again" : "Approve with passkey"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );

  return { admit, sheet };
}
