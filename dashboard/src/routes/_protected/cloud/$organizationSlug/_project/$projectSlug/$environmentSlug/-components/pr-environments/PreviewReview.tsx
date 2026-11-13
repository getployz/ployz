import { Suspense, useState } from "react";
import type { PreviewReviewView, Review, Standing } from "@ployz/sdk";
import { KeyRoundIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";
import { deployScope, type DeployScope } from "#/modules/approval-session/approval-session";
import { previewReviewQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { answerReview, draftReview, REVIEW_CAPABILITIES, ReviewRefused, shownText } from "#/modules/github/preview-review";
import { olderSays } from "#/modules/runtime/daemon-compat";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useEnvironmentMachines } from "#/providers/runtime-provider";

/** Approve `head`, with a review of it for each PR Environment that runs under a preview rule. */
export type Approve = (head: string, reviews?: { environment: string; review: Review }[]) => Promise<void>;

/**
 * `standing`: a head approved or trusted already has only a review left to do, as after its last review expired, or
 * when the preview rule doesn't trust its author.
 */
type Props = { organizationSlug: string; project: string; environment: string; number: number; head: string; approve: Approve; standing: Standing };

/**
 * Approve deploy for a pull request whose PR Environment is open: the person reviews the commit first and their passkey
 * answers the review.
 */
export function ApproveDeploy(props: Props) {
  return (
    <Suspense fallback={props.standing === "needs_approval" ? <Button size="sm" variant="outline" disabled>Review and approve</Button> : null}>
      <Reviewing {...props} />
    </Suspense>
  );
}

function Reviewing({ organizationSlug, project, environment, number, head, approve, standing }: Props) {
  const read = useStoreView(organizationSlug, previewReviewQuery({ project, environment }));
  const view = read.ok ? read.value : null;
  const { machines } = useRuntimeLens(organizationSlug);
  const environmentMachines = useEnvironmentMachines(view?.namespace ?? "");
  const [open, setOpen] = useState(false);
  const scope = view === null ? null : deployScope(
    { environmentId: view.environment.id, environmentName: view.environment.name, namespace: view.namespace },
    environmentMachines, machines, REVIEW_CAPABILITIES,
  );
  if (view === null || scope === null) return null;
  return (
    <>
      <Button size="sm" variant="outline" onClick={() => setOpen(true)}>{standing === "needs_approval" || standing === "trusted" ? "Review and approve" : "Review again"}</Button>
      {open ? <ReviewDialog view={view} scope={scope} number={number} head={head} approve={approve} onClose={() => setOpen(false)} /> : null}
    </>
  );
}

/** The review as it would be signed, then one passkey prompt. */
function ReviewDialog({ view, scope, number, head, approve, onClose }: {
  view: PreviewReviewView; scope: DeployScope; number: number; head: string; approve: Approve; onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const cluster = scope.scope?.cluster ?? null;
  // What the dialog shows: the review as it would be signed now. The press drafts it again, at its own moment.
  let lines: readonly string[] = [];
  let problem = scope.older !== null ? olderSays(scope.older, (server) => `${server} needs a newer Ployz to review previews. Upgrade it first.`)
    : cluster === null ? "Cloud couldn't check the servers. Try again."
    : view.commit !== head ? "This pull request has a newer commit. Review that one."
    : null;
  if (problem === null && cluster !== null) {
    try {
      lines = draftReview(view, cluster).lines;
    } catch (error) {
      problem = error instanceof ReviewRefused ? error.message : "This browser couldn't draft the review. Try again.";
    }
  }

  const sign = async () => {
    if (busy || problem !== null || cluster === null) return;
    setBusy(true);
    setFailed(null);
    try {
      const review = await answerReview(draftReview(view, cluster));
      await approve(view.commit, [{ environment: view.environment.id, review }]);
      onClose();
    } catch (error) {
      // The Store's refusal toasts by itself; a passkey's or the draft's shows here.
      if (error instanceof ReviewRefused) setFailed(error.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open onOpenChange={(next) => { if (!next && !busy) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Review #{number} at <span className="font-mono">{view.commit.slice(0, 7)}</span></DialogTitle>
          <DialogDescription>
            Read this commit's changes on GitHub first. Your servers build and run only this commit, with these settings,
            in {shownText(view.environment.name)}.
          </DialogDescription>
        </DialogHeader>
        <ReviewLines lines={lines} />
        {problem !== null ? <p role="alert" className="text-sm text-destructive">{problem}</p>
          : failed !== null ? <p role="alert" className="text-sm text-destructive">{failed}</p> : null}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy} />}>Cancel</DialogClose>
          <Button onClick={() => void sign()} disabled={busy || problem !== null}>
            {busy ? <Spinner data-icon="inline-start" /> : <KeyRoundIcon data-icon="inline-start" />}Approve with passkey
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * The review's lines as ployz-core wrote them: each Service, then its settings indented. Spacing shows as written, so
 * `X="a b"` and `X="a  b"` read apart.
 */
export function ReviewLines({ lines }: { lines: readonly string[] }) {
  if (lines.length === 0) return null;
  return (
    <ul className="flex flex-col gap-1 text-sm">
      {lines.map((line, at) => line.startsWith("  ")
        ? <li key={`${at}:${line}`} className="pl-4 font-mono text-xs whitespace-pre-wrap break-all">{line.slice(2)}</li>
        : <li key={`${at}:${line}`} className="whitespace-pre-wrap break-words">{line}</li>)}
    </ul>
  );
}
