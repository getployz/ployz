"use client";

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { CircleDashedIcon, GitBranchPlusIcon, MoreVerticalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Kbd } from "#/components/ui/kbd";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { useIsMobile } from "#/hooks/use-mobile";
import { useStartingPoint } from "#/modules/branches/branch.collection";
import { useDeploymentAttempt, useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { activeStep, deploymentStatusLabel } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { presentRow } from "#/modules/branches/branch-review";
import { useBranchReview, type BranchReviewView } from "#/modules/branches/use-branch-review";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { EnvironmentChangesReview, StagedChanges } from "./EnvironmentChangesReview";

/**
 * Where the bottom bar renders: the scene, outside the canvas that turns inert under a panel. The canvas owns the change
 * state, so it portals the bar here.
 */
export const BottomBarSlot = createContext<HTMLElement | null>(null);

/**
 * Where a Branch's review page shows its staged changes ("Not deployed here yet"). The page sets the slot; the bar, which
 * owns the change actions, portals the staged-changes review into it. The bar also says why Merge and Update must wait
 * (`unsettled`): something staged or an active attempt; null when neither.
 */
export const StagedReviewSlot = createContext<{
  slot: HTMLElement | null; setSlot: (slot: HTMLElement | null) => void;
  unsettled: string | null; setUnsettled: (reason: string | null) => void;
}>({ slot: null, setSlot: () => {}, unsettled: null, setUnsettled: () => {} });

const BRANCH_REVIEW_ROUTE_TO = "/cloud/$organizationSlug/$projectSlug/$environmentSlug/review";

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
 * The bottom bar shows one thing at a time: a starting point's state, else staged changes, else a running or queued
 * attempt whose page isn't open, else on a Branch what would merge into its Parent, else what's new in its Parent, else
 * nothing. A starting point's staged nodes are the recipe Branches copy, not pending work.
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
  const { slot: reviewSlot, setUnsettled } = useContext(StagedReviewSlot);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const review = useBranchReview(params.organizationSlug, environmentId);
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const isMobile = useIsMobile();
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Newest first: the oldest active attempt holds, or is next for, the Environment execution slot.
  const active = useEnvironmentDeployments(params.organizationSlug, environmentId)
    .filter(({ deployment }) => isActiveDeployment(deployment.status)).reverse();
  const startingPoint = useStartingPoint(params.organizationSlug, environmentId);
  const hasChanges = !startingPoint && (totalChanges > 0 || canSaveWithoutDeploying);
  const deployable = hasChanges && canDeploy && totalChanges > 0;
  const shown = hasChanges ? null : active.find(({ deployment }) => deployment.id !== viewedId);
  // The server's gate (assertBranchSettled) is the same: an empty Environment Change Set and no active attempt.
  const unsettled = hasChanges ? "Deploy or discard the changes staged here first."
    : active.length ? "Wait for this branch's deployment to finish." : null;
  useEffect(() => setUnsettled(unsettled), [setUnsettled, unsettled]);

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

  const bar = startingPoint ? (
    <Bar icon={<CircleDashedIcon className="size-4 text-muted-foreground" />} title={`${startingPoint.name} isn't deployed`} detail="A starting point for branches">
      <Link to="/cloud/$organizationSlug/$projectSlug/$environmentSlug/new-branch" params={params} className={buttonVariants({ size: "sm" })}>
        <GitBranchPlusIcon data-icon="inline-start" />New branch
      </Link>
    </Bar>
  ) : hasChanges ? (
    <Bar staged title={totalChanges > 0 ? `${totalChanges} ${totalChanges === 1 ? "change" : "changes"}` : "Unpublished changes"} detail={stagedDetail(groups, totalChanges)}>
      {/* On a Branch, Review opens its review page, whose first section is this review. */}
      {review ? <ReviewLink label="Review" /> : <Button ref={triggerRef} size="sm" variant="outline" aria-expanded={open} onClick={openReview}>Review</Button>}
      {/* Deploying behind a running or queued attempt queues. */}
      <Button size="sm" disabled={!deployable} aria-keyshortcuts="Shift+Enter" onClick={deploy}>
        {active.length > 0 ? "Deploy next" : "Deploy"}{isMobile ? null : <Kbd>⇧+Enter</Kbd>}
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label="More change actions" title="More change actions" />}>
          <MoreVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="top" className="w-auto">
          <DropdownMenuItem variant="destructive" disabled={!groups.some((group) => group.canDiscard)} onClick={() => void onDiscardAll()}>
            Discard all changes
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </Bar>
  ) : shown ? <AttemptState environmentId={environmentId} deploymentId={shown.deployment.id} />
    : review ? <BranchState review={review} /> : null;

  const reviewProps = {
    groups, totalChanges, canDeploy: deployable, canSave: canSaveWithoutDeploying, commitMessage,
    onClose: () => setOpen(false), onCommitMessageChange, onDeploy: deploy,
    onSave: () => { setOpen(false); onSaveWithoutDeploying(); },
    onDiscardAll: async () => { if (await onDiscardAll()) setOpen(false); },
    onDiscardNode, onDiscardRow,
  };

  return (
    <>
      {bar && slot ? createPortal(bar, slot) : null}
      {open ? <EnvironmentChangesReview {...reviewProps} /> : null}
      {reviewSlot ? createPortal(hasChanges ? <StagedChanges inline {...reviewProps} />
        : <p className="text-sm text-muted-foreground">Nothing staged here.</p>, reviewSlot) : null}
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

/** A Branch with nothing staged or running: what would merge into its Parent, else what's new there. */
function BranchState({ review }: { review: BranchReviewView }) {
  const isMobile = useIsMobile();
  const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;
  const [first] = review.merge;
  if (first) {
    const row = presentRow(first, review.nameOf);
    return (
      <Bar title={`${plural(review.changes, "change")} for ${review.parent.name}`}
        detail={`${row.node}${row.label ? ` · ${row.label}` : ""}${row.after ? ` ${row.before ? `${row.before} → ` : ""}${row.after}` : ""}`}>
        <ReviewLink label={isMobile ? "Review" : "Review and merge"} />
      </Bar>
    );
  }
  if (review.updates === 0) return null;
  return (
    <Bar title={`${plural(review.updates, "update")} from ${review.parent.name}`} detail={null}>
      <ReviewLink label="Review" />
    </Bar>
  );
}

function ReviewLink({ label }: { label: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  return <Link to={BRANCH_REVIEW_ROUTE_TO} params={params} className={buttonVariants({ size: "sm", variant: "outline" })}>{label}</Link>;
}

/**
 * A running or queued attempt: its status and message, where it is, and Logs to open its Deployment Page. Read like the
 * page reads it (with its build tail), so both name the same status.
 */
function AttemptState({ environmentId, deploymentId }: { environmentId: string; deploymentId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { attempt } = useDeploymentAttempt(params.organizationSlug, environmentId, deploymentId, { buildLog: true });
  if (!attempt) return null;
  const { deployment, nodes, view } = attempt;
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
