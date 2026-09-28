"use client";

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { CircleDashedIcon, GitBranchIcon, GitBranchPlusIcon, GitPullRequestIcon, MoreVerticalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Kbd } from "#/components/ui/kbd";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { useIsMobile } from "#/hooks/use-mobile";
import { useBranchUnsettled, useStartingPoint } from "#/modules/branches/branch.collection";
import { useUpdateBranch } from "#/modules/branches/branch-commands";
import { useDeploymentAttempt, useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { activeStep, deploymentStatusLabel } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { listNames, plural } from "#/modules/branches/branch-plan";
import { useBranchReview, type BranchReviewView, type PullRequest } from "#/modules/branches/use-branch-review";
import { useHeldChanges, useStagedInstead } from "#/modules/pr-environments/conditional-save.collection";
import { HeldChanges, waitingLine } from "../branch-review/HeldChanges";
import { SaveSheet } from "../branch-review/SaveSheet";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_NEW_BRANCH_ROUTE_TO, ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";
import { bottomBarRows } from "./bottom-bar-rows";

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
 * The bottom bar: one row for the Environment itself (a starting point, else changes to deploy, else a running or queued
 * attempt whose page isn't open, else changes held here for a pull request), and on a Branch a second row for its Parent
 * (changes to save, else updates). See bottomBarRows. A starting point's staged nodes are the recipe Branches copy, not
 * pending work.
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
  const review = useBranchReview(params.organizationSlug, environmentId);
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const isMobile = useIsMobile();
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Newest first: the oldest active attempt holds, or is next for, the Environment execution slot.
  const active = useEnvironmentDeployments(params.organizationSlug, environmentId)
    .filter(({ deployment }) => isActiveDeployment(deployment.status)).reverse();
  const startingPoint = useStartingPoint(params.organizationSlug, environmentId);
  const hasChanges = !startingPoint && (totalChanges > 0 || canSaveWithoutDeploying);
  const deployable = hasChanges && canDeploy && totalChanges > 0;
  const shown = hasChanges ? undefined : active.find(({ deployment }) => deployment.id !== viewedId);
  // Held for a pull request, not staged here: the bar's last state, so the list stays reachable.
  const [waiting] = useHeldChanges(params.organizationSlug, environmentId);
  // Rows a pull request's merge staged here instead of saving, marked in the review.
  const stagedInstead = useStagedInstead(params.organizationSlug, environmentId);
  const rows = bottomBarRows({
    startingPoint: startingPoint !== undefined, staged: hasChanges, attempt: shown !== undefined, waiting: waiting !== undefined,
    branch: review && { pullRequest: review.pullRequest !== null, changes: review.changes, updates: review.updates },
  });

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

  const own = rows.own === "starting_point" && startingPoint ? (
    <Row icon={<CircleDashedIcon className="size-4 text-muted-foreground" />} title={`${startingPoint.name} isn't deployed`} detail="A starting point for branches">
      <Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO} params={params} className={buttonVariants({ size: "sm" })}>
        <GitBranchPlusIcon data-icon="inline-start" />New branch
      </Link>
    </Row>
  ) : rows.own === "staged" ? (
    <Row staged title={totalChanges > 0 ? `${plural(totalChanges, "change")} to deploy` : "Unpublished changes"} detail={stagedDetail(groups, totalChanges)}>
      <Button ref={triggerRef} size="sm" variant="outline" aria-expanded={open} onClick={openReview}>Details</Button>
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
    </Row>
  ) : rows.own === "attempt" && shown ? <AttemptState environmentId={environmentId} deploymentId={shown.deployment.id} />
    : rows.own === "waiting" && waiting ? (
      <Row icon={<GitPullRequestIcon className="size-4 text-muted-foreground" />} title={waitingLine(waiting).title} detail={waitingLine(waiting).detail}>
        <Button ref={triggerRef} size="sm" variant="outline" aria-expanded={open} onClick={openReview}>Details</Button>
      </Row>
    ) : null;
  const parent = !review ? null
    : rows.parent === "save" ? (
      <Row icon={<GitBranchIcon className="size-4 text-muted-foreground" />} title={`${plural(review.changes, "change")} to save`} detail={`into ${review.parent.name}`}>
        <Button size="sm" variant="outline" onClick={() => setSaving(true)}>Save</Button>
      </Row>
    ) : rows.parent === "update" ? <UpdatesState review={review} environmentId={environmentId} />
    : rows.parent === "pull_request" && review.pullRequest ? <PrEnvironmentState review={review} pullRequest={review.pullRequest} environmentId={environmentId} />
    : null;
  const bar = own || parent ? (
    <div role="group" aria-label="Bottom bar" className="bottom-bar">{own}{parent}</div>
  ) : null;

  const reviewProps = {
    groups, totalChanges, canDeploy: deployable, canSave: canSaveWithoutDeploying, commitMessage,
    onClose: () => setOpen(false), onCommitMessageChange, onDeploy: deploy,
    onSave: () => { setOpen(false); onSaveWithoutDeploying(); },
    onDiscardAll: async () => { if (await onDiscardAll()) setOpen(false); },
    onDiscardNode, onDiscardRow, held: waiting || stagedInstead.length ? <HeldChanges environmentId={environmentId} /> : null,
  };

  return (
    <>
      {bar && slot ? createPortal(bar, slot) : null}
      {open ? <EnvironmentChangesReview {...reviewProps} /> : null}
      {saving && review ? <SaveSheet review={review} branchId={environmentId} onClose={() => setSaving(false)} /> : null}
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

/** A PR Environment's second row: what goes to its Destinations, and where its check stands. */
function PrEnvironmentState({ review, pullRequest, environmentId }: { review: BranchReviewView; pullRequest: PullRequest; environmentId: string }) {
  const isMobile = useIsMobile();
  if (review.changes === 0) return <UpdatesState review={review} environmentId={environmentId} />;
  const landings = review.goesTo.filter((landing) => landing.rows.length);
  // Approved: every Destination with changes has a standing approval. The check may still want a value.
  const approved = landings.every((landing) => landing.approval);
  const to = listNames(landings.map((landing) => landing.destination.name));
  return (
    <Row title={pullRequest.closed ? `#${pullRequest.number} is closed` : approved ? "Approved" : `${plural(review.changes, "change")} for ${to}`}
      detail={pullRequest.closed ? null : !approved ? "Not approved yet"
        : review.check?.passing ? `Lands when #${pullRequest.number} merges` : review.check?.reason ?? null}>
      <ReviewLink label={isMobile || approved || pullRequest.closed ? "Review" : "Review and approve"} />
    </Row>
  );
}

/**
 * Updates from the Parent: Update stages them here. While the Branch deploys or has changes to deploy, or when only Live
 * Nodes changed, Details opens the review page, which says why.
 */
function UpdatesState({ review, environmentId }: { review: BranchReviewView; environmentId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { update } = useUpdateBranch(params.organizationSlug);
  const unsettled = useBranchUnsettled(params.organizationSlug, environmentId);
  if (review.updates === 0) return null;
  return (
    <Row title={`${plural(review.updates, "update")} from ${review.parent.name}`} detail={null}>
      {unsettled || review.update.length === 0 ? <ReviewLink label="Details" />
        : <Button size="sm" variant="outline" onClick={() => update(environmentId)}>Update</Button>}
    </Row>
  );
}

function ReviewLink({ label }: { label: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  return <Link to={ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO} params={params} className={buttonVariants({ size: "sm", variant: "outline" })}>{label}</Link>;
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
    <Row icon={<DeploymentStatusIcon status={view.status} />} title={`${deploymentStatusLabel(view)} · ${deployment.message ?? "Deployment"}`} detail={step?.text ?? null}>
      <Link
        to={DEPLOYMENT_PAGE_ROUTE_TO}
        params={{ ...params, deploymentId: deployment.id }}
        search={step?.nodeId ? { service: step.nodeId } : {}}
        className={buttonVariants({ size: "sm", variant: "secondary" })}
      >
        Logs
      </Link>
    </Row>
  );
}

function Row({ staged = false, icon, title, detail, children }: {
  staged?: boolean; icon?: ReactNode; title: string; detail: string | null; children: ReactNode;
}) {
  return (
    <div className="bottom-bar-row" data-staged={staged || undefined}>
      {icon ? <span className="ml-1.5 flex">{icon}</span> : null}
      <div className="min-w-0 flex-1 px-1.5 text-xs">
        <p className={staged ? "truncate font-medium text-changed-deep tabular-nums" : "truncate font-medium"}>{title}</p>
        {detail ? <p className="truncate text-muted-foreground">{detail}</p> : null}
      </div>
      {children}
    </div>
  );
}
