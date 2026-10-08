import { useEffect, useState, type KeyboardEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRightIcon, XIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from "#/components/ui/card";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Kbd } from "#/components/ui/kbd";
import { Marker, MarkerContent } from "#/components/ui/marker";
import { Textarea } from "#/components/ui/textarea";
import type { ApprovalDecision } from "#/modules/approvals/approvals";
import { approvalOptions, decideApproval, landDecision, refetchApprovals, type ApprovalView } from "./approvals.queries";
import { approvalSubject, destroyedLine, otherChanges } from "./approval-review";

/**
 * One destructive Publish, Deploy or Server operation waiting on a human, drawn from its Cloud row and reread while it waits, so a
 * reload, another tab or the CLI's own answer all show the same card. `onSettled` runs once the row stops waiting.
 */
export function ApprovalCard({ organizationSlug, id, seed, autoFocus, onSettled }: {
  organizationSlug: string;
  id: string;
  seed?: ApprovalView;
  autoFocus?: boolean;
  onSettled?: () => void;
}) {
  const { data: approval } = useQuery({ ...approvalOptions(organizationSlug, id), initialData: seed });
  const settled = approval !== undefined && approval.status !== "pending";
  useEffect(() => {
    if (settled) onSettled?.();
  }, [settled, onSettled]);

  if (approval === undefined) return null;
  switch (approval.status) {
    case "pending": return <PendingCard organizationSlug={organizationSlug} approval={approval} autoFocus={autoFocus} />;
    case "approved": return <Marker><MarkerContent>Approved. {approvalSubject(approval.command, approval.review).verb} goes ahead.</MarkerContent></Marker>;
    case "denied": return <Marker><MarkerContent>Denied{approval.reason ? `: ${approval.reason}` : "."}</MarkerContent></Marker>;
    case "superseded": return <Marker><MarkerContent>The plan changed since this was asked. Ask again to review the new one.</MarkerContent></Marker>;
  }
}

function PendingCard({ organizationSlug, approval, autoFocus }: { organizationSlug: string; approval: ApprovalView; autoFocus?: boolean }) {
  const queryClient = useQueryClient();
  const [reason, setReason] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const { effects } = approval.review;
  const subject = approvalSubject(approval.command, approval.review);
  const others = otherChanges(approval.review);

  const decide = async (decision: ApprovalDecision) => {
    if (sending) return;
    setSending(true);
    setRefused(null);
    const answer = await decideApproval(approval.id, decision);
    setSending(false);
    if (answer.ok) return landDecision(queryClient, organizationSlug, approval.id, answer.decided);
    setRefused(answer.message);
    return refetchApprovals(queryClient, organizationSlug);
  };
  const approve = () => decide({ approve: { digest: approval.digest } });
  const deny = () => decide({ reject: reason?.trim() ? { reason: reason.trim() } : {} });
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void (reason === null ? approve() : deny());
    } else if (event.key === "Escape") {
      event.preventDefault();
      void deny();
    }
  };

  return (
    <Card state="destructive" size="sm" tabIndex={0} autoFocus={autoFocus} onKeyDown={onKeyDown}
      aria-label={`${subject.verb} ${subject.name} needs approval`}
      className="outline-none focus-visible:ring-2">
      <CardHeader className="flex items-center justify-between gap-2">
        <CardTitle>{subject.verb} {subject.name}</CardTitle>
        <Badge variant="destructive">Destroys {effects.length} {effects.length === 1 ? "thing" : "things"}</Badge>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        <ul aria-label="Destroys" className="flex flex-col gap-1 rounded-lg bg-destructive/10 px-2.5 py-2 text-xs">
          {effects.map((effect) => {
            const line = destroyedLine(effect);
            return (
              <li key={`${effect.kind}:${effect.path}`} className="flex gap-2">
                <XIcon aria-hidden className="mt-0.5 size-3 shrink-0 text-destructive" />
                <span>{line.verb} <b>{line.name}</b>{line.after}</span>
              </li>
            );
          })}
        </ul>
        {others.length > 0 && (
          <Collapsible>
            <CollapsibleTrigger className="group flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
              <ChevronRightIcon aria-hidden className="size-3 transition-transform group-data-panel-open:rotate-90" />
              {others.length} other {others.length === 1 ? "change" : "changes"}
            </CollapsibleTrigger>
            <CollapsibleContent>
              <ul className="flex flex-col gap-0.5 pt-1 pl-4 font-mono text-xs text-muted-foreground">
                {others.map((change) => <li key={change.key}>{change.mark} {change.node} {change.text}</li>)}
              </ul>
            </CollapsibleContent>
          </Collapsible>
        )}
        {reason !== null && (
          <Textarea autoFocus aria-label="Reason" placeholder="Why not? The agent reads this." value={reason}
            onChange={(event) => setReason(event.target.value)} className="min-h-14 text-xs" />
        )}
        {refused && <p role="alert" className="text-xs text-destructive">{refused}</p>}
      </CardContent>
      <CardFooter className="gap-1.5">
        <Button size="sm" variant="destructive" disabled={sending} onClick={() => void approve()}>
          Approve{reason === null && <Kbd>⌘↵</Kbd>}
        </Button>
        <Button size="sm" variant="outline" disabled={sending} onClick={() => void deny()}>
          Deny <Kbd>{reason === null ? "esc" : "⌘↵"}</Kbd>
        </Button>
        {reason === null && (
          <Button size="sm" variant="ghost" className="ml-auto text-muted-foreground" onClick={() => setReason("")}>
            Deny with a reason…
          </Button>
        )}
      </CardFooter>
    </Card>
  );
}
