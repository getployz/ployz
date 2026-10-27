import { useEffect, useId, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRightIcon, XIcon } from "lucide-react";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "#/components/ui/card";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { TraceLine } from "#/components/ui/trace";
import { Textarea } from "#/components/ui/textarea";
import { useMinute } from "#/hooks/use-minute";
import { cn } from "#/lib/utils";
import { approvalBadge, approvalSubject, type ApprovalDecision, destroyedLine, droppedLine, losesVolumeAccess, prunes, redeployLine } from "#/modules/approvals/approvals";
import { lossMoment } from "#/modules/volume-placement/volume-placement";
import { approvalOptions, decideApproval, landDecision, refetchApprovals, type ApprovalView } from "./approvals.queries";
import { type ChangeLine, reviewedChanges } from "./approval-review";

/**
 * One destructive Save, Deploy or Server operation waiting on a human, drawn from its Cloud row and reread while it waits, so a
 * reload, another tab or the CLI's own answer all show the same card. `onSettled` runs once the row stops waiting.
 */
export function ApprovalCard({ organizationSlug, id, seed, onSettled }: {
  organizationSlug: string;
  id: string;
  seed?: ApprovalView;
  onSettled?: () => void;
}) {
  const { data: approval } = useQuery({ ...approvalOptions(organizationSlug, id), initialData: seed });
  const settled = approval !== undefined && approval.status !== "pending";
  useEffect(() => {
    if (settled) onSettled?.();
  }, [settled, onSettled]);

  if (approval === undefined) return null;
  switch (approval.status) {
    case "pending": return <PendingCard organizationSlug={organizationSlug} approval={approval} />;
    case "approved": return <TraceLine>Approved. {approval.command === "publish" ? "Saves without deploying." : `${approvalSubject(approval.command, approval.review).verb} goes ahead.`}</TraceLine>;
    case "denied": return <TraceLine>Denied{approval.reason ? `: ${approval.reason}` : "."}</TraceLine>;
    case "superseded": return <TraceLine>The plan changed since this was asked. Ask again to review the new one.</TraceLine>;
  }
}

function PendingCard({ organizationSlug, approval }: { organizationSlug: string; approval: ApprovalView }) {
  const queryClient = useQueryClient();
  const [reason, setReason] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const { effects } = approval.review;
  const subject = approvalSubject(approval.command, approval.review);
  const redeploy = redeployLine(approval.command, approval.review);
  const place = subject.project === null ? subject.name : `${subject.project}/${subject.name}`;
  const { asked, others } = reviewedChanges(approval.command, approval.review);
  const waits = approval.protected && approval.yours;
  const now = useMinute() ?? new Date();

  const decide = async (decision: ApprovalDecision) => {
    if (sending) return;
    setSending(true);
    setRefused(null);
    const answer = await decideApproval(organizationSlug, approval.id, decision);
    setSending(false);
    if (answer.ok) return landDecision(queryClient, organizationSlug, approval.id, answer.decided);
    setRefused(answer.message);
    return refetchApprovals(queryClient, organizationSlug);
  };
  const approve = () => decide({ approve: { digest: approval.digest } });
  const deny = () => decide({ reject: reason?.trim() ? { reason: reason.trim() } : {} });

  return (
    <Card state="destructive" size="sm" aria-label={`${subject.verb} ${place} needs approval`}>
      <CardHeader className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 flex-col gap-0.5">
          <CardTitle>{subject.verb} {place}</CardTitle>
          <CardDescription>
            {approval.asker && !approval.yours && <>Asked by {approval.asker} · </>}<RelativeTime date={new Date(approval.created_at)} />
          </CardDescription>
        </div>
        <Badge variant="destructive">{approvalBadge(approval.command, approval.review)}</Badge>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        {redeploy !== null && <p className="text-xs">{redeploy}</p>}
        {(effects.length > 0 || prunes(approval.review)) && (
          <ul aria-label={losesVolumeAccess(approval.review) ? "Effects" : "Destroys"} className="flex flex-col gap-1 rounded-lg bg-destructive/10 px-2.5 py-2 text-xs">
            {effects.map((effect) => {
              const line = destroyedLine(effect, approval.command, approval.review);
              return (
                <li key={`${effect.kind}:${effect.path}`} className="flex gap-2">
                  <XIcon aria-hidden className="mt-0.5 size-3 shrink-0 text-destructive" />
                  <span>
                    {line.verb}{line.name !== null && <> <b>{line.name}</b></>}{line.after}
                    {effect.lost_after !== undefined && (
                      <span className="block">{`Changes after ${lossMoment(new Date(effect.lost_after.at * 1000), now)} are not in ${effect.lost_after.restored}`}</span>
                    )}
                  </span>
                </li>
              );
            })}
            {prunes(approval.review) && (
              <li className="flex gap-2">
                <XIcon aria-hidden className="mt-0.5 size-3 shrink-0 text-destructive" />
                <span>
                  Also removes any other service running in this environment
                  {droppedLine(approval.review) !== null && <span className="block">{droppedLine(approval.review)}</span>}
                </span>
              </li>
            )}
          </ul>
        )}
        {asked.length > 0 && <ChangeLines label="Saves" changes={asked} className="text-foreground" />}
        {others.length > 0 && (
          <Collapsible defaultOpen={effects.length === 0 && asked.length === 0}>
            <CollapsibleTrigger className="group flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
              <ChevronRightIcon aria-hidden className="size-3 transition-transform group-data-panel-open:rotate-90" />
              {others.length} {effects.length + asked.length > 0 ? "other " : ""}{others.length === 1 ? "change" : "changes"}
            </CollapsibleTrigger>
            <CollapsibleContent>
              <ChangeLines changes={others} className="pt-1 pl-4 text-muted-foreground" />
            </CollapsibleContent>
          </Collapsible>
        )}
        {reason !== null && (
          <Textarea autoFocus aria-label="Reason" placeholder="Why not? The agent reads this." value={reason}
            onChange={(event) => setReason(event.target.value)} className="min-h-14 text-xs" />
        )}
        {refused && <p role="alert" className="text-xs text-destructive">{refused}</p>}
      </CardContent>
      {waits ? (
        <CardFooter><p className="text-xs text-muted-foreground">Waiting on an owner or admin.</p></CardFooter>
      ) : <CardFooter className="gap-1.5">
        <Button size="sm" variant="destructive" disabled={sending} onClick={() => void approve()}>
          Approve
        </Button>
        <Button size="sm" variant="outline" disabled={sending} onClick={() => void deny()}>
          Deny
        </Button>
        {reason === null && (
          <Button size="sm" variant="ghost" className="ml-auto text-muted-foreground" onClick={() => setReason("")}>
            Deny with a reason…
          </Button>
        )}
      </CardFooter>}
    </Card>
  );
}

function ChangeLines({ label, changes, className }: { label?: string; changes: ChangeLine[]; className: string }) {
  const id = useId();
  const list = (
    <ul aria-labelledby={label ? id : undefined} className={cn("flex flex-col gap-0.5 text-xs", className)}>
      {changes.map((change) => (
        <li key={change.key}>
          {change.mark} {change.node} <span className={cn(change.variable && "font-mono")}>{change.name}</span>
          {change.values === "" ? null : <> <span className="font-mono">{change.values}</span></>}
        </li>
      ))}
    </ul>
  );
  if (!label) return list;
  return (
    <div className="flex flex-col gap-0.5">
      <p id={id} className="text-xs text-muted-foreground">{label}</p>
      {list}
    </div>
  );
}
