"use client";

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { CircleDashedIcon, GitBranchIcon, GitBranchPlusIcon, GitPullRequestIcon, MoreVerticalIcon, PowerOffIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Kbd } from "#/components/ui/kbd";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { useIsMobile } from "#/hooks/use-mobile";
import { useBranchUnsettled, useOff, useStartingPoint } from "#/modules/branches/branch.collection";
import { useUpdateBranch } from "#/modules/branches/branch-commands";
import { useDeploymentAttempt, useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { activeStep, deploymentStatusLabel } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { listNames, plural } from "#/modules/branches/branch-plan";
import { rowLineage } from "#/modules/branches/branch-review";
import { useBranchReview, type BranchReviewView, type PullRequest } from "#/modules/branches/use-branch-review";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useWaitingSaves } from "#/modules/pr-environments/conditional-save.collection";
import { useConditionalSave, usePrEnvironmentOff } from "#/modules/pr-environments/conditional-save-commands";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { useLandedNotes } from "../branch-review/landed-notes";
import { GoesLiveSheet, SaveSheet } from "../branch-review/SaveSheet";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_NEW_BRANCH_ROUTE_TO, ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO } from "../environment-route-paths";
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
 * attempt whose page isn't open, else Off, else changes that go live with a pull request), and on a Branch a row for its Parent
 * (changes to save, else updates), or on a PR Environment one per Destination (changes to save, or saved to go live with
 * the pull request). See bottomBarRows. A starting point's staged nodes are the recipe Branches copy, not pending work.
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
  // The Save sheet open, for the Parent (null) or a PR Environment's Destination (its index).
  const [saving, setSaving] = useState<{ destination: number | null } | null>(null);
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
  // Saved to go live with a pull request, not changes to deploy here: the bar's last state.
  const waiting = useWaitingSaves(params.organizationSlug, environmentId);
  const landed = useLandedNotes(environmentId, groups);
  const pr = review?.pullRequest ?? null;
  const off = useOff(params.organizationSlug, environmentId);
  const rows = bottomBarRows({
    startingPoint: startingPoint !== undefined, staged: hasChanges, attempt: shown !== undefined, off, waiting: waiting.length > 0,
    branch: review && {
      changes: review.changes, updates: review.updates,
      // A closed pull request takes no more saves.
      destinations: pr && (pr.closed ? [] : review.goesTo.map((landing) => ({ changes: landing.rows.length, saved: landing.saved !== null }))),
    },
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
    : rows.own === "off" ? <OffState environmentId={environmentId} />
    : rows.own === "waiting" ? <WaitingState saves={waiting} /> : null;
  // Saying where only when more than one Destination has something.
  const several = rows.parent.filter(({ destination }) => destination !== null).length > 1;
  const parent = !review ? [] : rows.parent.map(({ row, destination }) => {
    const landing = destination === null ? undefined : review.goesTo[destination];
    const key = `${row}:${destination}`;
    if (row === "update") return <UpdatesState key={key} review={review} environmentId={environmentId} />;
    if (row === "saved") return landing?.saved && pr ? <SavedState key={key} review={review} saved={landing.saved} pullRequest={pr} environmentId={environmentId} several={several} /> : null;
    return (
      <Row key={key} icon={<GitBranchIcon className="size-4 text-muted-foreground" />}
        title={`${plural(landing?.rows.length ?? review.changes, "change")} to save`}
        detail={!landing || !pr ? `into ${review.parent.name}`
          : `${several ? `into ${landing.destination.name} · ` : ""}go live when PR #${pr.number} merges`}>
        <Button size="sm" variant="outline" onClick={() => setSaving({ destination })}>Save</Button>
      </Row>
    );
  });
  const bar = own || parent.length ? (
    <div role="group" aria-label="Bottom bar" className="bottom-bar">{own}{parent}</div>
  ) : null;

  const reviewProps = {
    groups, totalChanges, canDeploy: deployable, canSave: canSaveWithoutDeploying, commitMessage,
    onClose: () => setOpen(false), onCommitMessageChange, onDeploy: deploy,
    onSave: () => { setOpen(false); onSaveWithoutDeploying(); },
    onDiscardAll: async () => { if (await onDiscardAll()) setOpen(false); },
    onDiscardNode, onDiscardRow, noteFor: landed.noteFor, landed: landed.rest,
  };

  return (
    <>
      {bar && slot ? createPortal(bar, slot) : null}
      {open ? <EnvironmentChangesReview {...reviewProps} /> : null}
      {saving && review ? (
        <SaveSheet review={review} branchId={environmentId} landing={saving.destination === null ? undefined : review.goesTo[saving.destination]}
          onClose={() => setSaving(null)} />
      ) : null}
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

const goLive = (n: number) => `${plural(n, "change")} go${n === 1 ? "es" : ""} live`;

/** A PR Environment's changes saved for one Destination: they go live when its pull request merges. Undo is under ⋮. */
function SavedState({ review, saved, pullRequest, environmentId, several }: {
  review: BranchReviewView; saved: ConditionalSaveRow; pullRequest: PullRequest; environmentId: string; several: boolean;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { withdraw } = useConditionalSave({
    organizationSlug: params.organizationSlug, prEnvironmentId: environmentId, destinationEnvironmentId: saved.destinationEnvironmentId, prNumber: pullRequest.number,
  });
  const [details, setDetails] = useState(false);
  const into = several ? ` · into ${review.environmentName(saved.destinationEnvironmentId)}` : "";
  return (
    <Row icon={<GitPullRequestIcon className="size-4 text-muted-foreground" />} title={goLive(saved.rows.length)} detail={`when PR #${pullRequest.number} merges${into}`}>
      <Button size="sm" variant="outline" onClick={() => setDetails(true)}>Details</Button>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label="More save actions" title="More save actions" />}>
          <MoreVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="top" className="w-auto">
          <DropdownMenuItem disabled={withdraw.isPending} onClick={() => withdraw.mutate()}>Undo</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {details ? (
        <GoesLiveSheet title={`${goLive(saved.rows.length)} when PR #${pullRequest.number} merges`} saves={[saved]}
          nameOf={(lineage) => review.nameOf(lineage)} onClose={() => setDetails(false)}
          actions={<Button variant="outline" disabled={withdraw.isPending} onClick={() => withdraw.mutate(undefined, { onSuccess: () => setDetails(false) })}>Undo</Button>} />
      ) : null}
    </Row>
  );
}

/** Off: shut down with its settings kept. The next push starts it again, or Deploy now. */
function OffState({ environmentId }: { environmentId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const document = useEnvironmentDocuments(params.organizationSlug).find((candidate) => candidate.id === environmentId);
  const { start } = usePrEnvironmentOff({ organizationSlug: params.organizationSlug, environmentId, name: document?.name ?? params.environmentSlug });
  return (
    <Row icon={<PowerOffIcon className="size-4 text-muted-foreground" />} title="Off" detail="Starts again on the next push">
      <Button size="sm" disabled={start.isPending} onClick={() => start.mutate()}>Deploy</Button>
    </Row>
  );
}

/** A Destination's quiet row: changes saved on PR Environments, which go live with their pull requests. Not changes to deploy. */
function WaitingState({ saves }: { saves: ConditionalSaveRow[] }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const lineageName = useLineageNames(params.organizationSlug);
  const documents = useEnvironmentDocuments(params.organizationSlug);
  const [details, setDetails] = useState(false);
  const nameOf = (lineage: string, prEnvironmentId: string | null) => lineageName(lineage, prEnvironmentId ?? undefined);
  const title = `${goLive(saves.reduce((n, save) => n + save.rows.length, 0))} with ${listNames(saves.map((save) => `PR #${save.prNumber}`))}`;
  const nodes = [...new Set(saves.flatMap((save) => save.rows.map(({ row }) => nameOf(rowLineage(row), save.prEnvironmentId))))];
  const prEnvironments = documents.filter((document) => saves.some((save) => save.prEnvironmentId === document.id));
  return (
    <Row icon={<GitPullRequestIcon className="size-4 text-muted-foreground" />} title={title} detail={nodes.join(", ")}>
      <Button size="sm" variant="outline" onClick={() => setDetails(true)}>Details</Button>
      {details ? (
        <GoesLiveSheet title={title} saves={saves} nameOf={nameOf} onClose={() => setDetails(false)}
          actions={prEnvironments.map((document) => (
            <Link key={document.id} to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: document.namespace }}
              className={buttonVariants({ variant: "outline" })}>Open {document.name}</Link>
          ))} />
      ) : null}
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
