"use client";

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { GitPullRequestIcon, MoreVerticalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import type { DeploymentSummary } from "@ployz/sdk";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { deploymentStatusIcons, deploymentStatusLabel, deploysLabel, uploadLabel, type ChangeGroup } from "#/modules/config-store/store-deployments";
import { listNames, plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { goLiveWhen } from "#/modules/config-store/store-pull-requests";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";
import { AddServerDialog } from "#/routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/add-server-dialog";

/**
 * Where the bottom bar renders: the scene, outside the canvas that turns inert under a panel. The canvas owns the change
 * state, so it portals the bar here.
 */
export const BottomBarSlot = createContext<HTMLElement | null>(null);

type BottomBarProps = {
  groups: ChangeGroup[];
  totalChanges: number;
  canPublish: boolean;
  /** Deploys, with the user's Deploy message (blank for none). */
  onDeploy: (message: string) => void;
  /** A Deploy is being admitted: Deploy waits, so a double click admits one. */
  admitting?: boolean;
  onPublish: () => void;
  onDiscardAll: () => void;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  /** The Environment's in-flight Deployments, newest first, whoever admitted them. */
  active: DeploymentSummary[];
  /** Details' notes: where changes came from, and merged pull requests' and the Parent's values. */
  notes: Pick<ReviewProps, "noteFor" | "neverSyncFor" | "after">;
  /** Changes open pull requests saved here, going live when each merges. */
  waiting?: ReadonlyArray<{ number: number; changes: number; environment: string }>;
  /** The Organization has no Server to deploy to: Deploy becomes Add a server; Publish still works. */
  noServers?: boolean;
};

type ReviewProps = Parameters<typeof EnvironmentChangesReview>[0];

/**
 * The bottom bar holds this Environment's own changes and nothing else, in one row like Railway's: changes to deploy
 * ("Apply 3 changes · Details · Deploy · ⋮"), else a running or queued Deployment whose page isn't open. What moves
 * between Environments, a Sync, is the Sync button's, at the canvas's top right; it lands in a bottom bar as changes to
 * deploy.
 */
export function BottomBar({
  groups,
  totalChanges,
  canPublish,
  onDeploy,
  onPublish,
  onDiscardAll,
  onDiscardNode,
  onDiscardRow,
  active: newestFirst,
  notes,
  waiting = [],
  noServers = false,
  admitting = false,
}: BottomBarProps) {
  const slot = useContext(BottomBarSlot);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Oldest first: the oldest holds, or is next for, the Environment's one run.
  const active = [...newestFirst].reverse();
  const hasChanges = totalChanges > 0 || canPublish;
  const deployable = totalChanges > 0 && !noServers;
  const shown = hasChanges ? undefined : active.find((deployment) => deployment.id !== viewedId);

  function deploy() {
    if (admitting) return;
    setOpen(false);
    onDeploy(message);
    setMessage("");
  }

  // New Services and Volumes go for good with a Discard: name them and ask first.
  const created = groups.filter((group) => group.lifecycle === "create" && group.canDiscard).map((group) => group.nodeName);

  // Discard shows at once and saves in the background, so the review closes with it.
  function discardAll() {
    setOpen(false);
    if (created.length > 0 && !confirmingDiscard) {
      setConfirmingDiscard(true);
      return;
    }
    setConfirmingDiscard(false);
    onDiscardAll();
  }

  function openReview() {
    workspaceRef.current = slot?.closest<HTMLElement>(".environment-canvas-scene") ?? null;
    setOpen(true);
  }

  useEffect(() => {
    if (wasOpen.current && !open) {
      (triggerRef.current ?? workspaceRef.current)?.focus({ preventScroll: true });
    }
    wasOpen.current = open;
  }, [open]);

  // ⇧+Enter deploys from anywhere except multi-line text, where it types a newline.
  useEffect(() => {
    if (!deployable) return;
    function deployOnShiftEnter(event: KeyboardEvent) {
      if (event.key !== "Enter" || !event.shiftKey || event.altKey || event.ctrlKey || event.metaKey || event.repeat || event.defaultPrevented) return;
      if (event.target instanceof HTMLTextAreaElement || (event.target instanceof HTMLElement && event.target.isContentEditable)) return;
      event.preventDefault();
      deploy();
    }
    document.addEventListener("keydown", deployOnShiftEnter);
    return () => document.removeEventListener("keydown", deployOnShiftEnter);
  });

  const row = hasChanges ? (
    <Row staged title={totalChanges > 0 ? `Apply ${plural(totalChanges, "change")}` : "Changes to publish"}
      shortTitle={totalChanges > 0 ? plural(totalChanges, "change") : "To publish"} detail={null}>
      <Button ref={triggerRef} variant="outline" aria-expanded={open} onClick={openReview}>Details</Button>
      {/* Deploying behind a running or queued attempt queues. */}
      {noServers ? (
        <AddServerDialog organizationSlug={params.organizationSlug} label="Add a server" variant="intent" />
      ) : <Tooltip>
        <TooltipTrigger render={<Button variant="intent" disabled={!deployable || admitting} aria-keyshortcuts="Shift+Enter" onClick={deploy} />}>
          {active.length > 0 ? "Deploy next" : "Deploy"}
        </TooltipTrigger>
        <TooltipContent>⇧+Enter</TooltipContent>
      </Tooltip>}
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon" variant="ghost" aria-label="More change actions" title="More change actions" />}>
          <MoreVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="top" className="w-auto">
          <DropdownMenuItem variant="destructive" disabled={!groups.some((group) => group.canDiscard)} onClick={discardAll}>
            Discard all changes
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </Row>
  ) : shown ? <AttemptState deployment={shown} /> : waiting.length ? (
    <Row icon={<GitPullRequestIcon className="size-4 text-muted-foreground" />}
      title={waiting.map(({ number }) => `PR #${number}`).join(", ")}
      detail={waiting.map(({ number, changes }) => goLiveWhen(changes, number)).join(" · ")}>
      {waiting.slice(0, 1).map(({ environment }) => (
        <Link key={environment} to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: environment }}
          className={buttonVariants({ variant: "outline" })}>
          Open {environment}
        </Link>
      ))}
    </Row>
  ) : null;
  const bar = row ? <div role="group" aria-label="Bottom bar" className="bottom-bar">{row}</div> : null;

  const reviewProps = {
    groups, totalChanges, canDeploy: deployable, canPublish,
    onClose: () => setOpen(false), onDeploy: deploy, message, onMessageChange: setMessage, admitting,
    onPublish: () => { setOpen(false); onPublish(); },
    onDiscardAll: discardAll,
    onDiscardNode, onDiscardRow,
    ...notes,
  };
  return (
    <>
      {bar && slot ? createPortal(bar, slot) : null}
      {open ? <EnvironmentChangesReview {...reviewProps} /> : null}
      <ConfirmDialog open={confirmingDiscard} onOpenChange={setConfirmingDiscard} title="Discard all changes?"
        description={`${listNames(created)} ${created.length === 1 ? "is" : "are"} new and will be deleted. Everything else goes back to how it is deployed.`}
        actionLabel="Discard all" variant="destructive" onConfirm={discardAll} />
    </>
  );
}

/** An in-flight Deployment, the CLI's or this dashboard's: its status, what it ships, and Logs to open its page. */
function AttemptState({ deployment }: { deployment: DeploymentSummary }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const deploys = deploysLabel(deployment);
  const detail = deploys && deployment.upload ? `${deploys} · ${uploadLabel(deployment.upload)}` : deploys;
  return (
    <Row icon={<DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />}
      title={`${deploymentStatusLabel(deployment)} · Deployment #${deployment.number}`} detail={detail}>
      <Link to={DEPLOYMENT_PAGE_ROUTE_TO} params={{ ...params, deploymentId: deployment.id }} className={buttonVariants({ variant: "outline" })}>
        Logs
      </Link>
    </Row>
  );
}


/** One row: what, in a few words, then its actions. Changes to deploy take the staged-intent surface. */
function Row({ staged = false, icon, title, shortTitle, detail, children }: {
  staged?: boolean; icon?: ReactNode; title: string; /** Fewer words on phones. */ shortTitle?: string; detail: string | null; children: ReactNode;
}) {
  return (
    <div className="bottom-bar-row" data-staged={staged || undefined}>
      {icon ? <span className="flex">{icon}</span> : null}
      <div className="min-w-0 flex-1 pr-3">
        <p className={cn(staged ? "truncate text-sm font-medium text-changed-deep tabular-nums" : "truncate text-sm font-medium", shortTitle && "max-sm:hidden")}>{title}</p>
        {shortTitle ? <p className="truncate text-sm font-medium text-changed-deep tabular-nums sm:hidden">{shortTitle}</p> : null}
        {detail ? <p className="truncate text-xs text-muted-foreground">{detail}</p> : null}
      </div>
      {children}
    </div>
  );
}
