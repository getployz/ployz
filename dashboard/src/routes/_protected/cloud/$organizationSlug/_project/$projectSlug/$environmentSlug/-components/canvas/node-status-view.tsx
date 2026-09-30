import { CloudOffIcon, TriangleAlertIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { RelativeTime, RunningTime } from "#/components/relative-time";
import { Skeleton } from "#/components/ui/skeleton";
import { plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { NodeOutcomeBadge } from "./NodeOutcomeBadge";
import type { Lit } from "../deployment-page";
import type { DeployChipState, NodeIssue, RuntimeLine, StagedColour, Tone } from "./node-status";

/**
 * How a node shows what the next Deploy does to it, beyond a card's own state: a tray's green, blue or red surface, and
 * its name struck when the Deploy removes it.
 */
export const STAGED_CLASSES = {
  success: { surface: "border-success-border bg-success-soft text-success", name: "" },
  info: { surface: "border-info-border bg-info-soft text-info", name: "" },
  destructive: { surface: "border-destructive-border bg-destructive-soft text-destructive", name: "line-through" },
} satisfies Record<StagedColour, { surface: string; name: string }>;

/** A Volume's fill by how full it is (`fillTone`): grey, then amber from 80%, red from 95%. */
export const FILL_CLASSES = {
  ok: { bar: "bg-border", text: "text-muted-foreground" },
  warn: { bar: "bg-warning-border", text: "text-warning" },
  bad: { bar: "bg-destructive-border", text: "text-destructive" },
} satisfies Record<"ok" | "warn" | "bad", { bar: string; text: string }>;

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
  bad: "bg-destructive ring-4 ring-destructive-soft",
  quiet: "bg-muted-foreground",
  idle: "border-[1.5px] border-muted-foreground",
  pending: "animate-pulse border-[1.5px] border-muted-foreground",
} satisfies Partial<Record<Tone, string>>;

const WORDS = {
  ok: "text-success", warn: "text-warning", bad: "text-destructive", crashed: "text-destructive",
  quiet: "text-muted-foreground", idle: "text-muted-foreground", pending: "text-muted-foreground", unreachable: "text-muted-foreground",
} satisfies Record<Tone, string>;

/** A status line's mark: a dot in its tone, the explosion for a crash, a cloud-off icon when the Servers can't be reached. */
function StatusMark({ tone }: { tone: Tone }) {
  if (tone === "crashed") return <CrashedIcon className="size-4 shrink-0 text-destructive" />;
  if (tone === "unreachable") return <CloudOffIcon className="size-3.5 shrink-0 text-muted-foreground" />;
  return <span className={cn("size-2 shrink-0 rounded-full", DOTS[tone])} />;
}

/** What runs now, in one word (grey with its age when it isn't current), ending in the node's ⚠ N, red when it's down. */
export function StatusLine({ status, issues, className }:
  { status: Pick<RuntimeLine, "word" | "tone" | "since">; issues: readonly NodeIssue[]; className?: string }) {
  return (
    <div className={cn("flex min-w-0 items-center gap-2", className)}>
      <StatusMark tone={status.tone} />
      {status.tone === "pending"
        ? <Skeleton className="h-3 w-20" aria-label="Checking" />
        : <span className={cn("min-w-0 truncate", WORDS[status.tone])}>
            {status.word}{status.since ? <> <RelativeTime date={status.since} /></> : null}
          </span>}
      {issues.length > 0 ? (
        <Badge variant={issues.some((issue) => issue.kind === "runtime" && issue.line.down) ? "destructive" : "warning"} className="ml-auto"
          aria-label={plural(issues.length, "issue")}>
          <TriangleAlertIcon data-icon="inline-start" />{issues.length}
        </Badge>
      ) : null}
    </div>
  );
}

/**
 * A node's chip: anything about Deploys. Under an open Deployment Page, its Node Outcome (`light`); else a Deploy in
 * flight, or what the next Deploy does.
 */
export function DeployChip({ light, chip }: { light: Lit | null | undefined; chip: DeployChipState | null }) {
  if (light !== undefined) return light ? <NodeOutcomeBadge light={light} /> : null;
  if (!chip) return null;
  if (chip.kind === "deploying") return <Badge variant="info">Deploying <RunningTime from={new Date(chip.since * 1000)} /></Badge>;
  if (chip.kind === "queued") return <Badge variant="secondary">Queued</Badge>;
  return <Badge variant={chip.variant}>{chip.label}</Badge>;
}
