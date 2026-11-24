import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRoundIcon } from "lucide-react";
import { ReviewDestroys } from "#/components/agent-sidebar/approval-card";
import { ProposalCard } from "#/components/agent-sidebar/proposal-card";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { PasskeyDeclined } from "#/modules/passkeys/passkey-statement";
import { fillDeployServerFn } from "#/modules/proposals/proposals.functions";
import { DEPLOY_MOVED, isDeploy } from "#/modules/proposals/proposal";
import { prunes } from "#/modules/approvals/approvals";
import { decideProposal, landProposal, proposalOptions } from "#/modules/proposals/proposals.queries";
import { useSealing } from "#/modules/sealing/use-sealing";
import { prepareDeployTapServerFn } from "./deploy-tap.functions";
import { type Prepared, prepareDeploy, signVersions } from "./deploy-tap";
import { SignedLines } from "./deploy-sheet";
import { fillPending } from "./fill-pending";

type Page =
  | { readonly kind: "reading" }
  | { readonly kind: "ready"; readonly prepared: Extract<Prepared, { kind: "admit" | "ask" }> }
  | { readonly kind: "stopped"; readonly message: string };

/**
 * The page a link opens: one proposal's Deploy, planned again as it is now with this person's own passkeys. A copied
 * password with no value yet is made here first, and the page reads the deploy again once it is. The person checks what
 * it destroys and what the passkey signs, and taps once, which signs and approves it. A plan that moved since it was asked for is denied, so the one who asked asks again.
 */
export function ApproveDeploy({ organizationSlug, id }: { organizationSlug: string; id: string }) {
  const queryClient = useQueryClient();
  const { data: proposal, isPending } = useQuery(proposalOptions(organizationSlug, id));
  const sealer = useRef(useSealing(organizationSlug).sealer);
  const [page, setPage] = useState<Page>({ kind: "reading" });
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState<"passkey" | "sending" | null>(null);
  const step = proposal?.steps[0];
  const waiting = proposal?.status === "presented" && step !== undefined && isDeploy(step);
  const command = waiting ? step.deploy.command : null;
  const review = waiting ? step.deploy.review?.review : undefined;

  useEffect(() => {
    if (command === null) return;
    let live = true;
    const stop = (message: string) => live && setPage({ kind: "stopped", message });
    const plan = async () => {
      const planned = await prepareDeployTapServerFn({
        data: { organizationSlug, environment: command.environment, services: [...command.services ?? []] },
      });
      // Each Pending value is made and sealed here, and lands only on the revision this Deploy was asked at. Cloud binds
      // the filled Deploy to the Store's review then, so the page reads the proposal again and plans that.
      const filled = await fillPending(command, planned, sealer.current, async (changes) => {
        const made = await fillDeployServerFn({ data: { organizationSlug, id, version: command.version ?? null, changes } });
        if (!made.ok) throw new Error(made.message);
        return made.revision;
      });
      if (filled !== null) {
        await queryClient.invalidateQueries({ queryKey: proposalOptions(organizationSlug, id).queryKey });
        return;
      }
      const prepared = await prepareDeploy(command, async () => planned);
      if (!live) return;
      if (prepared.kind === "stale") {
        await decideProposal(organizationSlug, id, { reject: { reason: DEPLOY_MOVED } });
        return stop(DEPLOY_MOVED);
      }
      setPage(prepared.kind === "refused" ? { kind: "stopped", message: prepared.message } : { kind: "ready", prepared });
    };
    plan().catch((error) => stop(error instanceof Error && error.message ? error.message : "Couldn't plan this deploy. Try again."));
    return () => { live = false; };
  }, [organizationSlug, id, command, queryClient]);

  if (isPending) return <Skeleton className="h-40 w-full" />;
  if (proposal === null || proposal === undefined) return <Note title="Nothing to approve here" body="This link doesn't name a deploy in this organization." />;
  if (!waiting) {
    return step !== undefined && !isDeploy(step) && proposal.status === "presented"
      ? <Note title="Not a deploy" body="Approve this one from the agent sidebar." />
      : <ProposalCard organizationSlug={organizationSlug} id={id} seed={proposal} />;
  }
  if (page.kind === "reading") return <Skeleton aria-label="Checking this deploy" className="h-40 w-full" />;
  if (page.kind === "stopped") return <Note title="Not approved" body={page.message} />;

  const { prepared } = page;
  const destroys = review !== undefined && (review.effects.length > 0 || prunes(review));
  const places = command?.services?.length ? command.services.join(", ") : "every service";
  const approve = async () => {
    setProblem(null);
    let versions;
    try {
      if (prepared.kind === "ask") {
        setBusy("passkey");
        versions = await signVersions(prepared.ask);
      }
    } catch (error) {
      setBusy(null);
      return setProblem(error instanceof PasskeyDeclined ? error.message
        : error instanceof Error && error.name === "NotAllowedError" ? "Your passkey didn't answer. Try again."
        : "Your browser couldn't use a passkey here.");
    }
    setBusy("sending");
    try {
      const answer = await decideProposal(organizationSlug, id, { approve: versions === undefined ? {} : { versions } });
      if (answer.ok) return await landProposal(queryClient, organizationSlug, id, answer.decided);
      setProblem(answer.message);
    } catch {
      setProblem("Couldn't reach Ployz Cloud. Try again.");
    } finally {
      setBusy(null);
    }
  };
  const deny = async () => {
    setBusy("sending");
    const answer = await decideProposal(organizationSlug, id, { reject: {} }).finally(() => setBusy(null));
    if (answer.ok) return await landProposal(queryClient, organizationSlug, id, answer.decided);
    setProblem(answer.message);
  };

  return (
    <Card aria-label="Approve this deploy">
      <CardHeader>
        <CardTitle>Deploy {places} to {proposal.project}/{proposal.environment}?</CardTitle>
        <CardDescription>
          {proposal.asker ? `${proposal.asker} asked for this deploy.` : "Someone asked for this deploy."}
          {prepared.kind === "ask" ? " Check each line. Your passkey signs exactly this." : ""}
          {review?.protected ? " Only an owner or admin can approve a deploy to a Protected Environment." : ""}
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {prepared.kind === "ask" && <SignedLines draft={prepared.ask.draft} />}
        {review !== undefined && <ReviewDestroys command="admit" review={review} />}
        {problem !== null && <p role="alert" className="text-sm text-destructive">{problem}</p>}
      </CardContent>
      <CardFooter className="gap-2">
        <Button variant={destroys ? "destructive" : "default"} disabled={busy !== null} onClick={() => void approve()}>
          {prepared.kind === "ask" && <KeyRoundIcon data-icon="inline-start" />}
          {busy === "passkey" ? "Waiting for your passkey…" : busy === "sending" ? "Deploying…" : prepared.kind === "ask" ? "Approve with passkey" : "Approve and deploy"}
        </Button>
        <Button variant="outline" disabled={busy !== null} onClick={() => void deny()}>Deny</Button>
      </CardFooter>
    </Card>
  );
}

function Note({ title, body }: { title: string; body: string }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{body}</CardDescription>
      </CardHeader>
    </Card>
  );
}
