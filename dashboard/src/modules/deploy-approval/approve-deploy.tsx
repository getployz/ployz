import { type ReactNode, useEffect, useRef, useState } from "react";
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
import { prunes } from "#/modules/deploy-approval/review";
import { decideProposal, landProposal, proposalOptions } from "#/modules/proposals/proposals.queries";
import { useSealing } from "#/modules/sealing/use-sealing";
import { prepareDeployTapServerFn } from "./deploy-tap.functions";
import { type Prepared, prepareDeploy, signVersions } from "./deploy-tap";
import { SignedLines } from "./deploy-sheet";
import { fillPending } from "./fill-pending";
import { RuleSummaryList } from "./rule-summary";
import { ownerLookup } from "./source-step";

type Page =
  | { readonly kind: "reading" }
  | { readonly kind: "ready"; readonly prepared: Extract<Prepared, { kind: "admit" | "ask" }> }
  /** `deniable`: the proposal still waits, and the person may deny it. */
  | { readonly kind: "stopped"; readonly message: string; readonly deniable: boolean };

/**
 * The page a link opens: one proposal's Deploy, planned again as it is now with this person's own passkeys. A copied
 * password with no value yet is made here first, and the page reads the deploy again once it is. The person checks what
 * it destroys and what the passkey signs, the automatic deploy rule included when a Service builds from source and no
 * rule covers it, and taps once, which signs and approves it. A plan that moved since it was asked for is denied, so the
 * one who asked asks again.
 */
export function ApproveDeploy({ organizationSlug, id }: { organizationSlug: string; id: string }) {
  const queryClient = useQueryClient();
  const { data: proposal, isPending } = useQuery(proposalOptions(organizationSlug, id));
  const sealer = useRef(useSealing(organizationSlug).sealer);
  const [page, setPage] = useState<Page>({ kind: "reading" });
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState<"passkey" | "sending" | null>(null);
  const step = proposal?.steps[0];
  // The one who asked, waiting on an owner or admin, plans nothing here: the card lets them withdraw it.
  const withdrawOnly = proposal?.mine === true && !proposal.decides;
  const waiting = proposal?.status === "presented" && step !== undefined && isDeploy(step) && !withdrawOnly;
  const command = waiting ? step.deploy.command : null;
  const review = waiting ? step.deploy.review?.review : undefined;

  useEffect(() => {
    if (command === null) return;
    let live = true;
    const stop = (message: string, deniable = true) => live && setPage({ kind: "stopped", message, deniable });
    const plan = async () => {
      const planned = await prepareDeployTapServerFn({
        data: { organizationSlug, environment: command.environment, services: [...command.services ?? []], rule: true },
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
      const prepared = await prepareDeploy(command, async () => planned, ownerLookup(queryClient, organizationSlug));
      if (!live) return;
      if (prepared.kind === "stale") {
        await decideProposal(organizationSlug, id, { reject: { reason: DEPLOY_MOVED } });
        return stop(DEPLOY_MOVED, false);
      }
      if (prepared.kind === "refused") return stop(prepared.message);
      // A Service only the rule builds doesn't build without it, so the page says why rather than approve it. The asker
      // heard it before any link, so only a change since or this person's own passkeys and Servers stop here. A rule
      // drafted here replaces the live one, so it must cover every shipped Git Service, as the decision checks.
      const drafted = prepared.kind === "ask" ? prepared.ask.draft.source?.summary.services.map(({ name }) => name) : undefined;
      const shipped = planned.kind !== "plan" || planned.tap.kind === "off" ? undefined : drafted === undefined ? planned.plan.sources : planned.plan.git ?? planned.plan.sources;
      const sources = shipped ?? [];
      const held = prepared.kind === "admit" ? prepared.held : prepared.ask.held;
      const sourced = new Set(drafted);
      const unbuilt = sources.find((service) => !sourced.has(service)) ?? null;
      if (held !== null && (!held.unchecked || unbuilt !== null)) return stop(held.reason);
      if (unbuilt !== null) return stop(`No automatic deploy rule can cover ${unbuilt} now, so your servers won't build it.`);
      setPage({ kind: "ready", prepared });
    };
    plan().catch((error) => stop(error instanceof Error && error.message ? error.message : "Couldn't plan this deploy. Try again."));
    return () => { live = false; };
  }, [organizationSlug, id, command, queryClient]);

  const deny = async () => {
    setBusy("sending");
    const answer = await decideProposal(organizationSlug, id, { reject: {} }).finally(() => setBusy(null));
    if (answer.ok) return await landProposal(queryClient, organizationSlug, id, answer.decided);
    setProblem(answer.message);
  };

  if (isPending) return <Skeleton className="h-40 w-full" />;
  if (proposal === null || proposal === undefined) return <Note title="Nothing to approve here" body="This link doesn't name anything waiting in this organization." />;
  // A stop or restart, or a decided proposal: its card approves or shows it.
  if (!waiting) return <ProposalCard organizationSlug={organizationSlug} id={id} seed={proposal} />;
  if (page.kind === "reading") return <Skeleton aria-label="Checking this deploy" className="h-40 w-full" />;
  if (page.kind === "stopped") {
    return (
      <Note title="Not approved" body={page.message}>
        {page.deniable && (
          <CardFooter className="flex-col items-start gap-2">
            {problem !== null && <p role="alert" className="text-sm text-destructive">{problem}</p>}
            <Button variant="outline" disabled={busy !== null} onClick={() => void deny()}>Deny</Button>
          </CardFooter>
        )}
      </Note>
    );
  }

  const { prepared } = page;
  const destroys = review !== undefined && (review.effects.length > 0 || prunes(review));
  const places = command?.services?.length ? command.services.join(", ") : "every service";
  const summary = prepared.kind === "ask" ? prepared.ask.draft.source?.summary : undefined;
  const approve = async () => {
    setProblem(null);
    let answer;
    try {
      if (prepared.kind === "ask") {
        setBusy("passkey");
        answer = await signVersions(prepared.ask);
      }
    } catch (error) {
      setBusy(null);
      return setProblem(error instanceof PasskeyDeclined ? error.message
        : error instanceof Error && error.name === "NotAllowedError" ? "Your passkey didn't answer. Try again."
        : "Your browser couldn't use a passkey here.");
    }
    setBusy("sending");
    try {
      const decided = await decideProposal(organizationSlug, id, { approve: answer ?? {} });
      if (decided.ok) return await landProposal(queryClient, organizationSlug, id, decided.decided);
      setProblem(decided.message);
    } catch {
      setProblem("Couldn't reach Ployz Cloud. Try again.");
    } finally {
      setBusy(null);
    }
  };

  return (
    <Card aria-label="Approve this deploy">
      <CardHeader>
        <CardTitle>Deploy {places} to {proposal.project}/{proposal.environment}?</CardTitle>
        <CardDescription>
          {proposal.asker ? `${proposal.asker} asked for this deploy.` : "Someone asked for this deploy."}
          {prepared.kind === "ask" ? " Check each line. Your passkey signs exactly this." : ""}
          {summary === undefined ? "" : ` Then each push to ${summary.branch} deploys the services below without asking.`}
          {review?.protected ? " Only an owner or admin can approve a deploy to a Protected Environment." : ""}
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {summary !== undefined && <RuleSummaryList summary={summary} />}
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

function Note({ title, body, children }: { title: string; body: string; children?: ReactNode }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{body}</CardDescription>
      </CardHeader>
      {children}
    </Card>
  );
}
