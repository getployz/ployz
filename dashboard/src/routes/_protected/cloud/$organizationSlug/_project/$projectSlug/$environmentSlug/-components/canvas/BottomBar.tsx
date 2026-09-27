"use client";

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { MoreVerticalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Kbd } from "#/components/ui/kbd";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { useIsMobile } from "#/hooks/use-mobile";
import { useEnvironmentDeployments, type DeploymentAttempt } from "#/modules/deployments/deployment.collection";
import { activeStep, deploymentStatusLabel } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
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
  environmentId: string;
  groups: CanvasEnvironmentChangeGroup[];
  totalChanges: number;
  canDeploy: boolean;
  commitMessage: string;
  canSaveWithoutDeploying: boolean;
  onCommitMessageChange: (value: string) => void;
  onDeploy: () => void;
  onSaveWithoutDeploying: () => void;
  onDiscardAll: () => Promise<boolean>;
  onDiscardNode: (group: CanvasEnvironmentChangeGroup) => void;
  onDiscardRow: (group: CanvasEnvironmentChangeGroup, path: string) => void;
};

/**
 * The bottom bar shows one thing at a time: staged changes, else a running or queued attempt whose page isn't open,
 * else nothing.
 */
export function BottomBar({
  environmentId,
  groups,
  totalChanges,
  canDeploy,
  commitMessage,
  canSaveWithoutDeploying,
  onCommitMessageChange,
  onDeploy,
  onSaveWithoutDeploying,
  onDiscardAll,
  onDiscardNode,
  onDiscardRow,
}: BottomBarProps) {
  const slot = useContext(BottomBarSlot);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const isMobile = useIsMobile();
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Newest first: the oldest active attempt holds, or is next for, the Environment execution slot.
  const active = useEnvironmentDeployments(params.organizationSlug, environmentId)
    .filter(({ deployment }) => isActiveDeployment(deployment.status)).reverse();
  const hasChanges = totalChanges > 0 || canSaveWithoutDeploying;
  const deployable = canDeploy && totalChanges > 0;
  const shown = hasChanges ? null : active.find(({ deployment }) => deployment.id !== viewedId);

  function deploy() {
    setOpen(false);
    onDeploy();
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

  const bar = hasChanges ? (
    <Bar staged title={totalChanges > 0 ? `${totalChanges} ${totalChanges === 1 ? "change" : "changes"}` : "Unpublished changes"} detail={stagedDetail(groups, totalChanges)}>
      <Button ref={triggerRef} size="sm" variant="outline" aria-expanded={open} onClick={openReview}>Review</Button>
      {/* Deploying behind a running or queued attempt queues. */}
      <Button size="sm" disabled={!deployable} aria-keyshortcuts="Shift+Enter" onClick={deploy}>
        {active.length > 0 ? "Deploy next" : "Deploy"}{isMobile ? null : <Kbd>⇧+Enter</Kbd>}
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label="More change actions" />}>
          <MoreVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="top" className="w-auto">
          <DropdownMenuItem variant="destructive" disabled={!groups.some((group) => group.canDiscard)} onClick={() => void onDiscardAll()}>
            Discard all changes
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </Bar>
  ) : shown ? <AttemptState attempt={shown} /> : null;

  return (
    <>
      {bar && slot ? createPortal(bar, slot) : null}
      {open ? <EnvironmentChangesReview
        groups={groups}
        totalChanges={totalChanges}
        canDeploy={deployable}
        canSave={canSaveWithoutDeploying}
        commitMessage={commitMessage}
        onClose={() => setOpen(false)}
        onCommitMessageChange={onCommitMessageChange}
        onDeploy={deploy}
        onSave={() => { setOpen(false); onSaveWithoutDeploying(); }}
        onDiscardAll={async () => { if (await onDiscardAll()) setOpen(false); }}
        onDiscardNode={onDiscardNode}
        onDiscardRow={onDiscardRow}
      /> : null}
    </>
  );
}

/** The one change ("api · Replicas 1 → 2"), or the changed services' names. */
function stagedDetail(groups: CanvasEnvironmentChangeGroup[], totalChanges: number) {
  const [first] = groups;
  const row = first?.rows[0];
  if (totalChanges === 1 && first && row) {
    return `${first.nodeName} · ${row.label} ${row.currentValue ? `${row.currentValue} → ` : ""}${row.newValue}`;
  }
  return groups.map((group) => group.nodeName).join(", ");
}

/** A running or queued attempt: its status and message, where it is, and Logs to open its Deployment Page. */
function AttemptState({ attempt: { deployment, nodes, view } }: { attempt: DeploymentAttempt }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const step = activeStep(deployment.runtimeProgress, nodes, view);
  return (
    <Bar icon={<DeploymentStatusIcon status={view.status} />} title={`${deploymentStatusLabel(view)} · ${deployment.message ?? "Deployment"}`} detail={step?.text ?? null}>
      <Link
        to={DEPLOYMENT_PAGE_ROUTE_TO}
        params={{ ...params, deploymentId: deployment.id }}
        search={step?.nodeId ? { service: step.nodeId } : {}}
        className={buttonVariants({ size: "sm", variant: "secondary" })}
      >
        Logs
      </Link>
    </Bar>
  );
}

function Bar({ staged = false, icon, title, detail, children }: {
  staged?: boolean; icon?: ReactNode; title: string; detail: string | null; children: ReactNode;
}) {
  return (
    <div role="group" aria-label="Bottom bar" className="bottom-bar" data-staged={staged || undefined}>
      {icon ? <span className="ml-1.5 flex">{icon}</span> : null}
      <div className="min-w-0 flex-1 px-1.5 text-xs">
        <p className={staged ? "truncate font-medium text-changed-deep tabular-nums" : "truncate font-medium"}>{title}</p>
        {detail ? <p className="truncate text-muted-foreground">{detail}</p> : null}
      </div>
      {children}
    </div>
  );
}
