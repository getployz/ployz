import { CloudOffIcon, PackageIcon, TerminalIcon, TriangleAlertIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Badge } from "#/components/ui/badge";
import { RelativeTime, RunningTime } from "#/components/relative-time";
import { Skeleton } from "#/components/ui/skeleton";
import { cn } from "#/lib/utils";
import { NodeOutcomeBadge } from "./NodeOutcomeBadge";
import type { Lit } from "../deployment-page";
import type { RuntimeLine, Tone, deployChip, nodeIssues } from "./node-status";

export function getServiceIcon(service: { source: { type: "empty" | "uploaded" | "git" | "image" } }) {
  switch (service.source.type) {
    case "empty":
    case "uploaded":
      return <TerminalIcon />;
    case "git":
      return <GitHubMarkIcon />;
    case "image":
      return <PackageIcon />;
  }
}

/** A crash: an explosion off the ground, like the one Railway draws. */
function CrashedIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" className={className} aria-hidden>
      <path d="M2.5 20.5 6 15.5 3 11.5l5 .5-1-5.5 4 3.5 1-5.5 1.8 5.3L18 5.5l-.8 5.8 4.3-.4-3.1 4.6 3.1 5" />
      <path d="M8.5 20.5l1.6-3.6 1.6 1.4 1.4-2.8 1.3 1.6 1.1 3.4" />
      <path d="M1.5 20.5h21" />
    </svg>
  );
}

const DOTS = {
  ok: "bg-success ring-4 ring-success-soft",
  warn: "bg-warning ring-4 ring-warning-soft",
  quiet: "bg-muted-foreground",
  idle: "border-[1.5px] border-muted-foreground",
  pending: "animate-pulse border-[1.5px] border-muted-foreground",
} satisfies Partial<Record<Tone, string>>;

const WORDS = {
  ok: "text-success", warn: "text-warning", bad: "text-destructive",
  quiet: "text-muted-foreground", idle: "text-muted-foreground", pending: "text-muted-foreground", unreachable: "text-muted-foreground",
} satisfies Record<Tone, string>;

/** A status line's mark: a dot in its tone, the explosion for a crash, a cloud-off icon when the Servers can't be reached. */
function StatusMark({ tone }: { tone: Tone }) {
  if (tone === "bad") return <CrashedIcon className="size-4 shrink-0 text-destructive" />;
  if (tone === "unreachable") return <CloudOffIcon className="size-3.5 shrink-0 text-muted-foreground" />;
  return <span className={cn("size-2 shrink-0 rounded-full", DOTS[tone])} />;
}

/** What runs now, in one word (grey with its age when it isn't current), ending in the node's ⚠ N. */
export function StatusLine({ status, issues, className }: { status: RuntimeLine; issues: ReturnType<typeof nodeIssues>; className?: string }) {
  return (
    <div className={cn("flex min-w-0 items-center gap-2", className)}>
      <StatusMark tone={status.tone} />
      {status.tone === "pending"
        ? <Skeleton className="h-3 w-20" aria-label="Checking" />
        : <span className={cn("min-w-0 truncate", WORDS[status.tone])}>
            {status.word}{status.since ? <> <RelativeTime date={status.since} /></> : null}
          </span>}
      {issues ? (
        <Badge variant={issues.tone === "bad" ? "destructive" : "warning"} className="ml-auto" aria-label={`${issues.count} ${issues.count === 1 ? "issue" : "issues"}`}>
          <TriangleAlertIcon data-icon="inline-start" />{issues.count}
        </Badge>
      ) : null}
    </div>
  );
}

/**
 * A node's chip: anything about Deploys. Under an open Deployment Page, its Node Outcome (`light`); else a Deploy in
 * flight, or what the next Deploy does.
 */
export function DeployChip({ light, chip }: { light: Lit | null | undefined; chip: ReturnType<typeof deployChip> }) {
  if (light !== undefined) return light ? <NodeOutcomeBadge light={light} /> : null;
  if (!chip) return null;
  if (chip.kind === "deploying") return <Badge variant="info">Deploying <RunningTime from={new Date(chip.since * 1000)} /></Badge>;
  if (chip.kind === "queued") return <Badge variant="secondary">Queued</Badge>;
  return <Badge variant={chip.variant}>{chip.label}</Badge>;
}
