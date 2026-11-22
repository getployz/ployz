import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { XIcon } from "lucide-react";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "#/components/ui/card";
import { type StepProgress, stepLoss, stepTitle } from "#/modules/proposals/proposal";
import { denial, DenyReason, DenyWithReasonButton, DestroyedItem, DestroysList, ServerLine } from "./decision-parts";
import { decideProposal, landProposal, type ProposalView, proposalOptions, refetchProposals } from "#/modules/proposals/proposals.queries";

const stepsWord = (count: number) => `${count} ${count === 1 ? "step" : "steps"}`;

/**
 * One proposal: waiting on a person, with every step an agent staged in the order they run, approved at once; or decided, with where each step got. Drawn from its Cloud row and reread while it waits or runs.
 * `onDismiss` lets a decided card go.
 */
export function ProposalCard({ organizationSlug, id, seed, onDismiss }: {
  organizationSlug: string;
  id: string;
  seed?: ProposalView;
  onDismiss?: () => void;
}) {
  const { data: proposal } = useQuery({ ...proposalOptions(organizationSlug, id), initialData: seed });

  if (proposal === undefined || proposal === null) return null;
  switch (proposal.status) {
    case "draft": return null;
    case "presented": return <PendingProposal organizationSlug={organizationSlug} proposal={proposal} />;
    case "approved":
    case "denied": return <DecidedProposal proposal={proposal} onDismiss={onDismiss} />;
  }
}

const PROGRESS = {
  accepted: "Starting", running: "Running", done: "Done", failed: "Failed", unknown: "Unclear", waiting: "Waiting", not_run: "Not run",
} as const satisfies Record<StepProgress, string>;

/** A decided plan and how far it got: each step's state, with why where one failed. */
function DecidedProposal({ proposal, onDismiss }: { proposal: ProposalView; onDismiss?: () => void }) {
  const place = `${proposal.project}/${proposal.environment}`;
  const approved = proposal.status === "approved";
  return (
    <Card size="sm" aria-label={`${stepsWord(proposal.steps.length)} in ${place} ${approved ? "approved" : "denied"}`}>
      <CardHeader className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 flex-col gap-0.5">
          <CardTitle>{stepsWord(proposal.steps.length)} in {place}</CardTitle>
          <CardDescription className="truncate">
            {proposal.asker && <>Asked by {proposal.asker} · </>}<RelativeTime date={new Date(proposal.created_at)} />
          </CardDescription>
        </div>
        <div className="flex items-center gap-1">
          <Badge variant={approved ? "secondary" : "outline"}>{approved ? "Approved" : "Denied"}</Badge>
          {onDismiss && <Button size="icon-sm" variant="ghost" aria-label="Dismiss" onClick={onDismiss}><XIcon /></Button>}
        </div>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        <ol aria-label="Steps" className="flex flex-col gap-1.5 text-xs">
          {proposal.steps.map((step, index) => {
            const run = proposal.progress[index];
            const state = approved && run !== undefined ? run.state : null;
            return (
              <li key={index} className="flex gap-2">
                <span className="w-4 shrink-0 text-right tabular-nums text-muted-foreground">{index + 1}.</span>
                <span className="flex min-w-0 flex-col">
                  <span>{stepTitle(step)}</span>
                  {state !== null && (
                    <span className={state === "failed" || state === "unknown" ? "text-destructive" : "text-muted-foreground"}>
                      {PROGRESS[state]}{run?.reason && (state === "failed" || state === "unknown") ? `: ${run.reason}` : ""}
                    </span>
                  )}
                </span>
              </li>
            );
          })}
        </ol>
        {!approved && proposal.reason && <p className="text-xs text-muted-foreground">{proposal.reason}</p>}
      </CardContent>
    </Card>
  );
}

function PendingProposal({ organizationSlug, proposal }: { organizationSlug: string; proposal: ProposalView }) {
  const queryClient = useQueryClient();
  const [reason, setReason] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const place = `${proposal.project}/${proposal.environment}`;
  const losses = proposal.steps.flatMap((step, index) => {
    const loss = stepLoss(step);
    return loss === null ? [] : [{ key: index, ...loss }];
  });
  const destroys = losses.length > 0;

  const decide = async (decision: Parameters<typeof decideProposal>[2]) => {
    const answer = await decideProposal(organizationSlug, proposal.id, decision);
    setSending(false);
    if (answer.ok) return landProposal(queryClient, organizationSlug, proposal.id, answer.decided);
    setRefused(answer.message);
    return refetchProposals(queryClient, organizationSlug);
  };
  const begin = () => {
    if (sending) return false;
    setSending(true);
    setRefused(null);
    return true;
  };
  const approve = () => begin() && decide({ approve: {} });
  const deny = () => begin() && decide(denial(reason));

  return (
    <Card state={destroys ? "destructive" : undefined} size="sm" aria-label={`${stepsWord(proposal.steps.length)} in ${place} need approval`}>
      <CardHeader className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 flex-col gap-0.5">
          <CardTitle>{stepsWord(proposal.steps.length)} in {place}</CardTitle>
          <CardDescription className="truncate">
            {proposal.asker && <>Asked by {proposal.asker} · </>}<RelativeTime date={new Date(proposal.created_at)} />
          </CardDescription>
        </div>
        <Badge variant={destroys ? "destructive" : "secondary"}>Proposal</Badge>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        <ol aria-label="Steps" className="flex flex-col gap-1.5 text-xs">
          {proposal.steps.map((step, index) => (
            <li key={index} className="flex gap-2">
              <span className="w-4 shrink-0 text-right tabular-nums text-muted-foreground">{index + 1}.</span>
              <span className="flex min-w-0 flex-col">
                <span>{stepTitle(step)}</span>
                <ServerLine namespace={step.namespace} runtime={step.runtime} />
              </span>
            </li>
          ))}
        </ol>
        {destroys && (
          <DestroysList label="Destroys">
            {losses.map((loss) => <DestroyedItem key={loss.key}>{loss.verb} <b>{loss.name}</b>{loss.after}</DestroyedItem>)}
          </DestroysList>
        )}
        {reason !== null && (
          <DenyReason value={reason} onChange={setReason} />
        )}
        {refused && <p role="alert" className="text-xs text-destructive">{refused}</p>}
      </CardContent>
      <CardFooter className="gap-1.5">
        <Button size="sm" variant={destroys ? "destructive" : "default"} disabled={sending} onClick={() => void approve()}>
          Approve all
        </Button>
        <Button size="sm" variant="outline" disabled={sending} onClick={() => void deny()}>
          Deny
        </Button>
        {reason === null && <DenyWithReasonButton onClick={() => setReason("")} />}
      </CardFooter>
    </Card>
  );
}
