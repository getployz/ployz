"use client";

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { MoreVerticalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import type { DeploymentSummary } from "@ployz/sdk";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { deploymentStatusIcons, deploymentStatusLabels, targetsLabel, uploadLabel, type ChangeGroup } from "#/modules/config-store/store-deployments";
import { plural } from "#/lib/plural";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";

/**
 * Where the bottom bar renders: the scene, outside the canvas that turns inert under a panel. The canvas owns the change
 * state, so it portals the bar here.
 */
export const BottomBarSlot = createContext<HTMLElement | null>(null);

type BottomBarProps = {
  groups: ChangeGroup[];
  totalChanges: number;
  canPublish: boolean;
  onDeploy: () => void;
  onPublish: () => void;
  onDiscardAll: () => Promise<boolean>;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  /** The Environment's in-flight Deployments, newest first, whoever admitted them. */
  active: DeploymentSummary[];
  /** Details' notes from merged pull requests. */
  notes: Pick<ReviewProps, "noteFor" | "after">;
};

type ReviewProps = Parameters<typeof EnvironmentChangesReview>[0];

/**
 * The bottom bar holds this Environment's own changes and nothing else, in one row like Railway's: changes to deploy
 * ("Apply 3 changes · Details · Deploy · ⋮"), else a running or queued Deployment whose page isn't open. What moves
 * between Environments, Save and Update, is the Branch button's, at the canvas's top right; it lands in a bottom bar as
 * changes to deploy.
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
}: BottomBarProps) {
  const slot = useContext(BottomBarSlot);
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Oldest first: the oldest holds, or is next for, the Environment's one run.
  const active = [...newestFirst].reverse();
  const hasChanges = totalChanges > 0 || canPublish;
  const deployable = totalChanges > 0;
  const shown = hasChanges ? undefined : active.find((deployment) => deployment.id !== viewedId);

  function deploy() {
    setOpen(false);
    onDeploy();
  }

  // A second Discard all queued behind the first would find nothing to discard and fail, so clicks while one saves are
  // ignored. The ref guards re-entry; the state only renders both Discard buttons disabled.
  const discardingRef = useRef(false);
  const [discarding, setDiscarding] = useState(false);
  async function discardAll() {
    if (discardingRef.current) return;
    discardingRef.current = true;
    setDiscarding(true);
    try {
      if (await onDiscardAll()) setOpen(false);
    } finally {
      discardingRef.current = false;
      setDiscarding(false);
    }
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
    <Row staged title={totalChanges > 0 ? `Apply ${plural(totalChanges, "change")}` : "Changes to publish"} detail={null}>
      <Button ref={triggerRef} variant="outline" aria-expanded={open} onClick={openReview}>Details</Button>
      {/* Deploying behind a running or queued attempt queues. */}
      <Tooltip>
        <TooltipTrigger render={<Button variant="intent" disabled={!deployable} aria-keyshortcuts="Shift+Enter" onClick={deploy} />}>
          {active.length > 0 ? "Deploy next" : "Deploy"}
        </TooltipTrigger>
        <TooltipContent>⇧+Enter</TooltipContent>
      </Tooltip>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon" variant="ghost" aria-label="More change actions" title="More change actions" />}>
          <MoreVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="top" className="w-auto">
          <DropdownMenuItem variant="destructive" disabled={discarding || !groups.some((group) => group.canDiscard)} onClick={() => void discardAll()}>
            Discard all changes
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </Row>
  ) : shown ? <AttemptState deployment={shown} /> : null;
  const bar = row ? <div role="group" aria-label="Bottom bar" className="bottom-bar">{row}</div> : null;

  const reviewProps = {
    groups, totalChanges, canDeploy: deployable, canPublish,
    onClose: () => setOpen(false), onDeploy: deploy,
    onPublish: () => { setOpen(false); onPublish(); },
    onDiscardAll: () => void discardAll(), discarding,
    onDiscardNode, onDiscardRow,
    ...notes,
  };
  return (
    <>
      {bar && slot ? createPortal(bar, slot) : null}
      {open ? <EnvironmentChangesReview {...reviewProps} /> : null}
    </>
  );
}

/** An in-flight Deployment, the CLI's or this dashboard's: its status, what it ships, and Logs to open its page. */
function AttemptState({ deployment }: { deployment: DeploymentSummary }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const detail = deployment.upload ? `${targetsLabel(deployment)} · ${uploadLabel(deployment.upload)}` : targetsLabel(deployment);
  return (
    <Row icon={<DeploymentStatusIcon status={deploymentStatusIcons[deployment.status]} />}
      title={`${deploymentStatusLabels[deployment.status]} · Deployment #${deployment.number}`} detail={`Deploys ${detail}`}>
      <Link to={DEPLOYMENT_PAGE_ROUTE_TO} params={{ ...params, deploymentId: deployment.id }} className={buttonVariants({ variant: "outline" })}>
        Logs
      </Link>
    </Row>
  );
}

/** One row: what, in a few words, then its actions. Changes to deploy take the staged-intent surface. */
function Row({ staged = false, icon, title, detail, children }: {
  staged?: boolean; icon?: ReactNode; title: string; detail: string | null; children: ReactNode;
}) {
  return (
    <div className="bottom-bar-row" data-staged={staged || undefined}>
      {icon ? <span className="flex">{icon}</span> : null}
      <div className="min-w-0 flex-1 pr-3">
        <p className={staged ? "truncate text-sm font-medium text-changed-deep tabular-nums" : "truncate text-sm font-medium"}>{title}</p>
        {detail ? <p className="truncate text-xs text-muted-foreground">{detail}</p> : null}
      </div>
      {children}
    </div>
  );
}
