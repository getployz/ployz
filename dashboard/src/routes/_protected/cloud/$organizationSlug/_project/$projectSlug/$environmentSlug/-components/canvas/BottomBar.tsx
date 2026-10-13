"use client";

import { createContext, useContext, useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { MoreVerticalIcon, XIcon } from "lucide-react";
import { AgentDock } from "#/components/agent-sidebar/agent-dock";
import { Button } from "#/components/ui/button";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import type { DeploymentSummary, Included } from "@ployz/sdk";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { deploymentStatusIcons, deploymentStatusLabel, deploysLabel, uploadLabel, type ChangeGroup } from "#/modules/config-store/store-deployments";
import { listNames, plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";
import { AddServerDialog } from "#/routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/add-server-dialog";

/**
 * Where the bottom bar renders while the agent is closed: the scene, outside the canvas that turns inert under a panel.
 * The canvas owns the change state, so it portals the bar here, or into the open agent's dock.
 */
export const BottomBarSlot = createContext<HTMLElement | null>(null);

/** History stages counted by the scene: a stage replaces the changes, so the Change message typed for them goes. */
export const HistoryStages = createContext<{ count: number; staged: () => void }>({ count: 0, staged: () => {} });

const DEPLOYED_SHOWS_MS = 6_000;
/** Far more Deployments than one bar shows, so the oldest ids it forgets are long ended. */
const WATCHED_LIMIT = 100;

/** A Failed dismissed in another tab hides here too. */
const subscribeStorage = (onChange: () => void) => {
  window.addEventListener("storage", onChange);
  return () => window.removeEventListener("storage", onChange);
};

type BottomBarProps = {
  groups: ChangeGroup[];
  totalChanges: number;
  runtimeChanges?: number;
  canPublish: boolean;
  /** Deploys, with the user's Deploy message (blank for none). */
  onDeploy: (message: string) => void;
  /** A Deploy is being admitted: Deploy waits, so a double click admits one. */
  admitting?: boolean;
  onPublish: (message: string) => void;
  onDiscardAll: () => void;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  /** The Environment's in-flight Deployments, newest first, whoever admitted them. */
  active: DeploymentSummary[];
  /**
   * Its latest ended Deployments, newest first, or undefined until they load. The newest shows Failed until dismissed;
   * one seen in flight here shows how it ended.
   */
  ended?: DeploymentSummary[];
  /** A running Deploy's storage step, in words, while it moves or copies a Volume before its Services. */
  storage?: (deploymentId: string) => string | null;
  /** Details' notes: where changes came from, and the Parent's deployed values. */
  notes: Pick<ReviewProps, "noteFor" | "neverSyncFor" | "originFor" | "after">;
  /** Details' Included list: the sources Syncs included here, with Include newer changes and Remove. */
  proposals?: Pick<ReviewProps, "included" | "onRemoveIncluded" | "onIncludeNewer">;
  /** The Organization has no Server to deploy to: Deploy becomes Add a server; Publish still works. */
  noServers?: boolean;
  /** Secrets without a value: the next Deploy leaves them out. */
  notSet?: number;
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
  runtimeChanges = totalChanges,
  canPublish,
  onDeploy,
  onPublish,
  onDiscardAll,
  onDiscardNode,
  onDiscardRow,
  active: newestFirst,
  ended,
  storage = () => null,
  notes,
  proposals,
  noServers = false,
  admitting = false,
  notSet = 0,
}: BottomBarProps) {
  const slot = useContext(BottomBarSlot);
  const dock = useContext(AgentDock);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const [open, setOpen] = useState(false);
  const stages = useContext(HistoryStages).count;
  const [typed, setTyped] = useState({ message: "", stages });
  const message = typed.stages === stages ? typed.message : "";
  const setMessage = (message: string) => setTyped({ message, stages });
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Oldest first: the oldest holds, or is next for, the Environment's one run.
  const active = [...newestFirst].reverse();
  const hasChanges = totalChanges > 0 || canPublish;
  const deployable = runtimeChanges > 0 && !noServers;
  const shown = hasChanges ? undefined : active.find((deployment) => deployment.id !== viewedId);
  // Seen in flight here, or its page opened before it was listed (as Deploy does): how a Deployment ends shows only
  // when this bar watched it run. A failure shows from any tab, so a reload keeps it.
  const watched = useRef(new Set<string>());
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(new Set());
  const dismissedKey = `bottom-bar-dismissed:${params.organizationSlug}/${params.projectSlug}/${params.environmentSlug}`;
  const dismissedFailure = useSyncExternalStore(subscribeStorage, () => localStorage.getItem(dismissedKey), () => null);
  useEffect(() => {
    const seen = watched.current;
    for (const deployment of active) seen.add(deployment.id);
    if (viewedId !== null && ended !== undefined && !ended.some((deployment) => deployment.id === viewedId)) seen.add(viewedId);
    for (const id of seen) if (seen.size > WATCHED_LIMIT) seen.delete(id);
  }, [active, ended, viewedId]);
  const latest = active.length > 0 ? undefined : ended?.[0];
  const showsLatest = latest !== undefined && (latest.status === "failed" ? latest.id !== dismissedFailure : watched.current.has(latest.id));
  const outcome = shown || !showsLatest || dismissed.has(latest.id) || latest.id === viewedId ? undefined : latest;
  const dismiss = (deployment: DeploymentSummary) => {
    setDismissed((ids) => new Set([...ids, deployment.id]));
    if (deployment.status !== "failed") return;
    localStorage.setItem(dismissedKey, deployment.id);
  };
  const deployed = outcome?.status === "applied" ? outcome.id : null;
  useEffect(() => {
    if (deployed === null) return;
    const timer = setTimeout(() => setDismissed((ids) => new Set([...ids, deployed])), DEPLOYED_SHOWS_MS);
    return () => clearTimeout(timer);
  }, [deployed]);

  function deploy() {
    if (admitting) return;
    setOpen(false);
    onDeploy(message);
    setMessage("");
  }

  // New Services and Volumes go for good with a Discard: name them and ask first.
  const created = groups.filter((group) => group.lifecycle === "create" && group.canDiscard).map((group) => group.nodeName);

  function discardAll() {
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

  const staged = hasChanges ? (
    <Row staged title={(totalChanges > 0 ? plural(totalChanges, "change") : "Changes to save") + (notSet > 0 ? ` · ${notSet} not set` : "")}
      shortTitle={totalChanges > 0 ? plural(totalChanges, "change") : "To save"} detail={null}>
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
  ) : shown ? <AttemptState deployment={shown} storage={storage(shown.id)} /> : null;
  // A failed Deploy hands its unreached changes back: how it ended stays above them until dismissed.
  const bar = staged || outcome ? (
    <div role="group" aria-label="Bottom bar" className="bottom-bar" data-docked={dock ? true : undefined}>
      {outcome ? <AttemptState deployment={outcome} storage={null} onDismiss={() => dismiss(outcome)} /> : null}
      {staged}
    </div>
  ) : null;
  const host = dock ?? slot;

  const includeNewer = proposals?.onIncludeNewer;
  const reviewProps = {
    environment: params.environmentSlug, groups, totalChanges, canDeploy: deployable, canPublish,
    onClose: () => setOpen(false), onDeploy: deploy, message, onMessageChange: setMessage, admitting,
    onPublish: () => onPublish(message),
    onDiscardAll: discardAll,
    onDiscardNode, onDiscardRow,
    ...notes,
    ...proposals,
    onIncludeNewer: includeNewer && ((included: Included) => {
      setOpen(false);
      includeNewer(included);
    }),
  };
  return (
    <>
      {bar && host ? createPortal(bar, host) : null}
      {open ? <EnvironmentChangesReview {...reviewProps} /> : null}
      <ConfirmDialog open={confirmingDiscard} onOpenChange={setConfirmingDiscard} title="Discard all changes?"
        description={`${listNames(created)} ${created.length === 1 ? "is" : "are"} new and will be deleted. Other changes return to their reviewed baseline.`}
        actionLabel="Discard all" variant="destructive" onConfirm={discardAll} />
    </>
  );
}

function AttemptState({ deployment, storage, onDismiss }: { deployment: DeploymentSummary; storage: string | null; onDismiss?: () => void }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const deploys = deploysLabel(deployment);
  const detail = storage ?? (deploys && deployment.upload ? `${deploys} · ${uploadLabel(deployment.upload)}` : deploys);
  return (
    <Row icon={<DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />}
      title={`${deploymentStatusLabel(deployment)} · Deployment #${deployment.number}`} detail={detail}>
      <Link to={DEPLOYMENT_PAGE_ROUTE_TO} params={{ ...params, deploymentId: deployment.id }} className={buttonVariants({ variant: "outline" })}>
        Logs
      </Link>
      {onDismiss ? (
        <Button size="icon" variant="ghost" aria-label="Dismiss" title="Dismiss" onClick={onDismiss}><XIcon /></Button>
      ) : null}
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
