import { useState, type ReactNode } from "react";
import { useParams } from "@tanstack/react-router";
import {
  ArrowDownIcon, ArrowUpIcon, ChevronDownIcon, CircleCheckIcon, ClockIcon, GitPullRequestIcon, PowerOffIcon, TriangleAlertIcon,
} from "lucide-react";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { cn } from "#/lib/utils";
import { useBranchUnsettled, useKeepBranch } from "#/modules/branches/branch.collection";
import { useUpdateBranch } from "#/modules/branches/branch-commands";
import type { BranchNews } from "#/modules/branches/branch-news";
import { plural } from "#/modules/branches/branch-plan";
import { presentRow, type ChangeRow } from "#/modules/branches/branch-review";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { useConditionalSave } from "#/modules/pr-environments/conditional-save-commands";
import { usePrEnvironmentOff } from "#/modules/pr-environments/off-commands";
import type { ConditionalSaveRow, PrShutdown } from "#/modules/pr-environments/tables";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { ChangeRowItem } from "./ChangeRowItem";
import { PrSaveSheet, SaveSheet } from "./SaveSheet";

type Branch = { review: BranchReviewView; environmentId: string; name: string };

/**
 * The Branch's news, most pressing first: the first leads in a card with the panel's one solid button, the rest are a
 * line each. A line with changes opens to them. Then the pull request's check on GitHub.
 */
export function BranchNewsList({ news, ...branch }: Branch & { news: BranchNews[] }) {
  const { review } = branch;
  const pr = review.pullRequest;
  return (
    <ItemGroup className="gap-2">
      {news.map((item, index) => <NewsItem key={newsKey(item)} news={item} lead={index === 0} {...branch} />)}
      {review.check && pr && !pr.closed ? (
        <NewsRow icon={review.check.passing ? <CircleCheckIcon className="text-success" /> : <TriangleAlertIcon className="text-warning" />}
          title={review.check.passing ? "Ready to merge on GitHub" : "Not ready to merge on GitHub"} detail={review.check.reason} />
      ) : null}
    </ItemGroup>
  );
}

const newsKey = (news: BranchNews) => news.kind === "save" || news.kind === "saved" ? `${news.kind}:${news.landing?.destination.id ?? "parent"}` : news.kind;

function NewsItem({ news, lead, review, environmentId, name }: Branch & { news: BranchNews; lead: boolean }) {
  switch (news.kind) {
    case "save": return <SaveNews lead={lead} review={review} environmentId={environmentId} landing={news.landing} count={news.count} />;
    case "saved": return <SavedNews lead={lead} review={review} environmentId={environmentId} landing={news.landing} saved={news.saved} />;
    case "update": return <UpdateNews lead={lead} review={review} environmentId={environmentId} count={news.count} />;
    case "shutdown_failed": return <ShutdownRow lead={lead} environmentId={environmentId} name={name} shutdown="failed" />;
    case "shutting_down": return <ShutdownRow lead={lead} environmentId={environmentId} name={name} shutdown="running" />;
    case "off": return <ShutdownRow lead={lead} environmentId={environmentId} name={name} shutdown="off" />;
    case "closing": return <ClosingNews lead={lead} environmentId={environmentId} days={news.days} />;
    case "up_to_date": return <NewsRow lead={lead} icon={<CircleCheckIcon className="text-success" />} title={`Up to date with ${review.parent.name}`} />;
  }
}

/** One line of news: what, in a few words, its one action, and its changes to open. The lead is a card. */
function NewsRow({ lead = false, icon, title, detail, action, changes }: {
  lead?: boolean;
  icon: ReactNode;
  title: string;
  detail?: ReactNode;
  action?: ReactNode;
  changes?: ReactNode[];
}) {
  const [open, setOpen] = useState(false);
  const text = (
    <>
      <ItemMedia variant="icon">{icon}</ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>
          {title}
          {changes?.length ? <ChevronDownIcon aria-hidden="true" className={cn("size-4 text-muted-foreground transition-transform", !open && "-rotate-90")} /> : null}
        </ItemTitle>
        {detail ? <ItemDescription className="truncate">{detail}</ItemDescription> : null}
      </ItemContent>
    </>
  );
  const row = (
    <Item variant={lead ? "outline" : "default"} size="sm">
      {changes?.length ? (
        <CollapsibleTrigger render={<button type="button" className="flex min-w-0 flex-1 items-center gap-2.5 text-left" />}>{text}</CollapsibleTrigger>
      ) : text}
      {action ? <ItemActions>{action}</ItemActions> : null}
      {changes?.length ? <CollapsibleContent className="basis-full"><ItemGroup className="gap-1">{changes}</ItemGroup></CollapsibleContent> : null}
    </Item>
  );
  return changes?.length ? <Collapsible open={open} onOpenChange={setOpen}>{row}</Collapsible> : row;
}

/** The lead's action is the panel's one solid button. */
const actionVariant = (lead: boolean) => lead ? "default" : "outline";

/** "web, api": the nodes the rows touch, once each. */
const nodeNames = (rows: ChangeRow[], review: BranchReviewView) => [...new Set(rows.map((row) => presentRow(row, review.nameOf).node))].join(", ");

/** What Save would put in the Parent, or a PR Environment's changes for one Destination, going live with the merge. */
function SaveNews({ lead, review, environmentId, landing, count }: {
  lead: boolean; review: BranchReviewView; environmentId: string; landing: BranchReviewView["goesTo"][number] | null; count: number;
}) {
  const [saving, setSaving] = useState(false);
  const pr = review.pullRequest;
  const into = landing?.destination.name ?? review.parent.name;
  const rows = landing?.rows ?? review.save;
  return (
    <>
      <NewsRow lead={lead} icon={<ArrowUpIcon />} title={`${plural(count, "change")} to save`}
        detail={pr ? `${nodeNames(rows, review)} · go live when #${pr.number} merges` : nodeNames(rows, review)}
        action={<Button size="sm" variant={actionVariant(lead)} onClick={() => setSaving(true)}>Save to {into}</Button>}
        changes={rows.map((row) => (
          <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)} conflict={row.role === "move" && row.conflict ? into : undefined} />
        ))} />
      {saving ? landing && pr
        ? <PrSaveSheet review={review} branchId={environmentId} landing={landing} pullRequest={pr} onClose={() => setSaving(false)} />
        : <SaveSheet review={review} branchId={environmentId} onClose={() => setSaving(false)} /> : null}
    </>
  );
}

/** A PR Environment's saved changes for one Destination: they go live when its pull request merges, until Undo. */
function SavedNews({ lead, review, environmentId, landing, saved }: {
  lead: boolean; review: BranchReviewView; environmentId: string; landing: BranchReviewView["goesTo"][number]; saved: ConditionalSaveRow;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { withdraw } = useConditionalSave({
    organizationSlug: params.organizationSlug, prEnvironmentId: environmentId, destinationEnvironmentId: landing.destination.id,
  });
  return (
    <NewsRow lead={lead} icon={<GitPullRequestIcon />} title={`Saved for ${landing.destination.name}`}
      detail={`${plural(saved.rows.length, "change")} · go live when #${saved.prNumber} merges`}
      action={<Button size="sm" variant="outline" disabled={withdraw.isPending} onClick={() => withdraw.mutate()}>Undo</Button>}
      changes={saved.rows.map(({ row }) => <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)} />)} />
  );
}

/**
 * The Parent's deployed changes the Branch doesn't have, and Live Nodes their owner redeployed since it last deployed.
 * Update stages the first here; the live ones need only the Branch's next deploy. While it must wait, it says why.
 */
function UpdateNews({ lead, review, environmentId, count }: { lead: boolean; review: BranchReviewView; environmentId: string; count: number }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { update } = useUpdateBranch(params.organizationSlug);
  const unsettled = useBranchUnsettled(params.organizationSlug, environmentId);
  const here = review.environmentName(environmentId);
  const conflicts = review.update.filter((row) => row.role === "move" && row.conflict).length;
  const names = [nodeNames(review.update, review), ...review.live.map((live) => review.nameOf(live.lineageId))].filter(Boolean).join(", ");
  return (
    <NewsRow lead={lead} icon={<ArrowDownIcon />} title={`${plural(count, "update")} from ${review.parent.name}`}
      detail={unsettled ?? (review.update.length ? <>
        {names}{conflicts ? <span className="text-warning"> · {conflicts} changed in {here} too</span> : null}
      </> : `${names} · Deploy to pick them up`)}
      action={!unsettled && review.update.length
        ? <Button size="sm" variant={actionVariant(lead)} onClick={() => update(environmentId)}>Update</Button> : null}
      changes={[
        ...review.update.map((row) => (
          <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)} conflict={row.role === "move" && row.conflict ? here : undefined} />
        )),
        ...review.live.map((live) => (
          <ChangeRowItem key={`live:${live.lineageId}`}
            row={{ key: live.lineageId, lineageId: live.lineageId, node: review.nameOf(live.lineageId), label: "Used live", before: "", after: "" }}
            description={<ItemDescription>{review.environmentName(live.ownerEnvironmentId)} deployed it <RelativeTime date={live.deployedAt} /></ItemDescription>} />
        )),
      ]} />
  );
}

/** A PR Environment's shutdown: what it's doing, Shut down again once it failed, and Deploy to start it once it's Off. */
export function ShutdownRow({ lead = false, environmentId, name, shutdown }: {
  lead?: boolean; environmentId: string; name: string; shutdown: PrShutdown;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { shutDown, start } = usePrEnvironmentOff({ organizationSlug: params.organizationSlug, environmentId, name });
  switch (shutdown) {
    case "running": return <NewsRow lead={lead} icon={<PowerOffIcon />} title="Shutting down" detail="Deploy once it's off" />;
    case "failed": return (
      <NewsRow lead={lead} icon={<PowerOffIcon className="text-destructive" />} title="Shutdown failed" detail="Some services may still run"
        action={<Button size="sm" variant={actionVariant(lead)} disabled={shutDown.isPending} onClick={() => shutDown.mutate()}>Shut down again</Button>} />
    );
    case "off": return (
      <NewsRow lead={lead} icon={<PowerOffIcon />} title="Off" detail="Starts again on the next push"
        action={<Button size="sm" variant={actionVariant(lead)} disabled={start.isPending} onClick={() => start.mutate()}>Deploy {name}</Button>} />
    );
  }
}

/** A Branch about to close itself for want of a deploy, with Keep. */
function ClosingNews({ lead, environmentId, days }: { lead: boolean; environmentId: string; days: number }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const keepBranch = useKeepBranch(params.organizationSlug);
  return (
    <NewsRow lead={lead} icon={<ClockIcon className="text-warning" />} title={`Closes in ${plural(days, "day")}`} detail="Without a deploy"
      action={<Button size="sm" variant={actionVariant(lead)} onClick={() => keepBranch(environmentId, true)}>Keep it</Button>} />
  );
}
