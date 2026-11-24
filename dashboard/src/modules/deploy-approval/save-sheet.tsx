import { useRef, useState } from "react";
import type { SavedRule } from "@ployz/sdk";
import { KeyRoundIcon } from "lucide-react";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { StoreRefused } from "#/lib/refusal";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { githubRepositoryOwnerQueryOptions } from "#/modules/github/github.queries";
import { PasskeyDeclined } from "#/modules/passkeys/passkey-statement";
import { SignedLines } from "./deploy-sheet";
import { prepareSaveTapServerFn } from "./deploy-tap.functions";
import { RuleSummaryList } from "./rule-summary";
import { type Answered, type OwnerOf, prepareSave, type Publish, type SaveAsk, type SaveSteps, saveWith, sendRule, signRule } from "./save-tap";

/** A Save that was not sent: refused before any prompt, or cancelled. Whoever said why already showed it. */
class SaveNotSent extends Error {}

type Open = { ask: SaveAsk; handles: readonly string[]; settle: (failed: Error | null) => void; problem: string | null; rule?: SavedRule };

/**
 * Saves from this tab. When what the Save saves isn't what the kept automatic deploy rule approves, the sheet shows the
 * rule and one passkey tap signs it with the Save. A caller's accepted retry of the same review, after the Store asked
 * to confirm a Volume loss, sends that answer again. `drop` forgets it when the caller cancels.
 */
export function useSaveTap(organizationSlug: string) {
  const writer = useStoreWriter(organizationSlug);
  const { queryClient } = useCollectionScope();
  const [open, setOpen] = useState<Open | null>(null);
  const [busy, setBusy] = useState<"passkey" | "saving" | null>(null);
  const answered = useRef<Answered | null>(null);

  const ownerOf: OwnerOf = async (repository, repositoryId) => {
    try {
      const owner = await queryClient.fetchQuery(githubRepositoryOwnerQueryOptions(organizationSlug, repository));
      return owner.repositoryId === repositoryId ? { ownerId: owner.ownerId }
        : { problem: `${repository} now names another repository. Connect your services to it again, then save.` };
    } catch {
      return { problem: `Couldn't read ${repository} on GitHub.` };
    }
  };

  const steps: SaveSteps = {
    answered,
    commit: (command, handles) => writer.commit(command, handles).then(() => undefined),
    prepare: (command) => prepareSave(command, (gate) => prepareSaveTapServerFn({ data: { organizationSlug, environment: command.environment, gate } }), ownerOf)
      .catch(() => ({ kind: "refused", message: "Couldn't check automatic deploys. Try again." }) as const),
    ask: async (ask, handles) => {
      const failed = await new Promise<Error | null>((settle) => setOpen({ ask, handles, settle, problem: null }));
      if (failed !== null) throw failed;
    },
    held: (reason, unchecked) => toast.warning(unchecked ? "Saved. Pushes may wait for approval." : "Saved. Pushes wait for approval.", { description: reason }),
    refused: (message) => {
      toast.error(`${message} Nothing saved.`);
      return new SaveNotSent();
    },
  };

  const save = (command: Publish, handles: readonly string[] = []) => saveWith(steps, command, handles);

  function close(sheet: Open, failed: Error | null) {
    sheet.settle(failed);
    setOpen(null);
  }

  async function approve(current: Open) {
    let rule = current.rule;
    try {
      if (rule === undefined) {
        setBusy("passkey");
        try {
          rule = await signRule(current.ask);
        } catch (error) {
          const problem = error instanceof PasskeyDeclined ? error.message
            : error instanceof Error && error.name === "NotAllowedError" ? "Your passkey didn't answer. Try again."
            : "Your browser couldn't use a passkey here.";
          return setOpen({ ...current, problem });
        }
      }
      setBusy("saving");
      await sendRule(steps, current.ask.command, { review: current.ask.review, rule }, current.handles);
      close(current, null);
    } catch (error) {
      // The Store refused it: the caller asks its own question, or the writer already said why.
      if (error instanceof StoreRefused) return close(current, error);
      // The Store may have saved it: a resend carries the same Save and rule, and a stale one is refused.
      setOpen({ ...current, rule, problem: "Couldn't reach Ployz Cloud. Try again." });
    } finally {
      setBusy(null);
    }
  }

  const summary = open?.ask.draft.source.summary;
  const sheet = open === null || summary === undefined ? null : (
    <Dialog open onOpenChange={(next) => { if (!next && busy === null) close(open, new SaveNotSent()); }}>
      <DialogContent className="flex max-h-[85dvh] flex-col sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Deploy every push to {summary.branch}?</DialogTitle>
          <DialogDescription>
            Each push to {summary.branch} deploys these settings with no prompt.
          </DialogDescription>
        </DialogHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
          <RuleSummaryList summary={summary} />
          <SignedLines draft={open.ask.draft} />
        </div>
        {open.problem === null ? null : <p role="alert" className="text-sm text-destructive">{open.problem}</p>}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy !== null} />}>Cancel</DialogClose>
          <Button disabled={busy !== null} onClick={() => void approve(open)}>
            <KeyRoundIcon data-icon="inline-start" />
            {busy === "passkey" ? "Waiting for your passkey…" : busy === "saving" ? "Saving…" : open.rule !== undefined ? "Try again" : "Save with passkey"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );

  return { save, drop: () => { answered.current = null; }, sheet };
}
