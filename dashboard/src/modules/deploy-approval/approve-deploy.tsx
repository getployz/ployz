import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRoundIcon } from "lucide-react";
import { DestroyedItem, DestroysList } from "#/components/agent-sidebar/decision-parts";
import { ProposalCard } from "#/components/agent-sidebar/proposal-card";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { PasskeyDeclined } from "#/modules/passkeys/passkey-statement";
import { fillDeployServerFn } from "#/modules/proposals/proposals.functions";
import { DEPLOY_MOVED, isDeploy } from "#/modules/proposals/proposal";
import { decideProposal, landProposal, proposalOptions } from "#/modules/proposals/proposals.queries";
import { useSealing } from "#/modules/sealing/use-sealing";
import { prepareDeployTapServerFn } from "./deploy-tap.functions";
import { type Prepared, prepareDeploy, signVersions } from "./deploy-tap";
import type { DeployPlan } from "./deploy-tap.server";
import { SignedLines } from "./deploy-sheet";
import { fillPending } from "./fill-pending";

type Page =
  | { readonly kind: "reading" }
  | { readonly kind: "ready"; readonly prepared: Extract<Prepared, { kind: "admit" | "ask" }>; readonly losses: readonly string[] }
  | { readonly kind: "stopped"; readonly message: string };

/** What the plan takes away: each node it removes, and each Volume whose data goes. */
const lossesOf = (planned: DeployPlan | null, command: AdmitDeploy) => [
  ...(planned?.kind === "plan" ? planned.plan.changes : []).flatMap((change) => [
    ...change.lifecycle === "delete" ? [`Removes ${change.name}`] : [],
    ...change.data === "deleted" ? [`Deletes the data in ${change.name}`] : [],
  ]),
  ...(command.accept_volume_loss ?? []).map((name) => `Replaces the data in ${name}`),
];

/**
 * The page a link opens: one proposal's Deploy, planned again as it is now with this person's own passkeys. A copied
 * password with no value yet is made here first. The person checks what the passkey signs and taps once, which signs
 * and approves it. A plan that moved since it was asked for is denied, so the one who asked asks again.
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

  useEffect(() => {
    if (command === null) return;
    let live = true;
    let last: DeployPlan | null = null;
    const stop = (message: string) => live && setPage({ kind: "stopped", message });
    const read = async (asked: AdmitDeploy) => last = await prepareDeployTapServerFn({
      data: { organizationSlug, environment: asked.environment, services: [...asked.services ?? []] },
    });
    const plan = async () => {
      const planned = await read(command);
      // Each Pending value is made and sealed here, and lands only on the revision this Deploy was asked at.
      const filled = await fillPending(command, planned, sealer.current, async (changes) => {
        const made = await fillDeployServerFn({ data: { organizationSlug, id, changes } });
        if (!made.ok) throw new Error(made.message);
        return made.revision;
      });
      const prepared = await (filled === null ? prepareDeploy(command, async () => planned) : prepareDeploy(filled, read));
      if (!live) return;
      if (prepared.kind === "stale") {
        await decideProposal(organizationSlug, id, { reject: { reason: DEPLOY_MOVED } });
        return stop(DEPLOY_MOVED);
      }
      setPage(prepared.kind === "refused" ? { kind: "stopped", message: prepared.message } : { kind: "ready", prepared, losses: lossesOf(last, command) });
    };
    plan().catch((error) => stop(error instanceof Error && error.message ? error.message : "Couldn't plan this deploy. Try again."));
    return () => { live = false; };
  }, [organizationSlug, id, command]);

  if (isPending) return <Skeleton className="h-40 w-full" />;
  if (proposal === null || proposal === undefined) return <Note title="Nothing to approve here" body="This link doesn't name a deploy in this organization." />;
  if (!waiting) {
    return step !== undefined && !isDeploy(step) && proposal.status === "presented"
      ? <Note title="Not a deploy" body="Approve this one from the agent sidebar." />
      : <ProposalCard organizationSlug={organizationSlug} id={id} seed={proposal} />;
  }
  if (page.kind === "reading") return <Skeleton aria-label="Checking this deploy" className="h-40 w-full" />;
  if (page.kind === "stopped") return <Note title="Not approved" body={page.message} />;

  const { prepared, losses } = page;
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
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {prepared.kind === "ask" && <SignedLines draft={prepared.ask.draft} />}
        {losses.length > 0 && (
          <DestroysList label="Destroys">
            {losses.map((loss) => <DestroyedItem key={loss}>{loss}</DestroyedItem>)}
          </DestroysList>
        )}
        {problem !== null && <p role="alert" className="text-sm text-destructive">{problem}</p>}
      </CardContent>
      <CardFooter className="gap-2">
        <Button variant={losses.length > 0 ? "destructive" : "default"} disabled={busy !== null} onClick={() => void approve()}>
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
