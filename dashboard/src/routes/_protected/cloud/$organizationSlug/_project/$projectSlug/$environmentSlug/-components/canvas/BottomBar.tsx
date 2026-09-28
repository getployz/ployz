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
import { useBranchUnsettled, useShutdown, useStartingPoint } from "#/modules/branches/branch.collection";
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
import { useConditionalSave } from "#/modules/pr-environments/conditional-save-commands";
import { usePrEnvironmentOff } from "#/modules/pr-environments/off-commands";
import type { ConditionalSaveRow, PrShutdown } from "#/modules/pr-environments/tables";
import { LandedNote, LandedRest, useLandedNotes } from "../branch-review/landed-notes";
import { GoesLiveChanges, GoesLiveSheet, PrSaveSheet, SaveSheet } from "../branch-review/SaveSheet";
import { goLive } from "#/modules/pr-environments/pr-check";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_NEW_BRANCH_ROUTE_TO, ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";
import { bottomBarRows, type SaveRow } from "./bottom-bar-rows";

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
  // The Save sheet open: for the Parent, or a PR Environment's Destination (its id).
  const [saving, setSaving] = useState<string | null>(null);
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
  const lineageName = useLineageNames(params.organizationSlug);
  const shutdown = useShutdown(params.organizationSlug, environmentId);
  const rows = bottomBarRows({
    startingPoint: startingPoint?.name ?? null, staged: hasChanges, attempt: shown?.deployment.id ?? null, shutdown, waiting: waiting.length > 0,
    branch: review && { changes: review.changes, updates: review.updates, pullRequest: review.pullRequest, goesTo: review.goesTo },
  });
  // Save opens on a row; the sheet shows only while that row is there, so a Destination that's gone closes it.
  const saveInto = saving === null ? undefined
    : rows.parent.find((row): row is SaveRow<BranchReviewView["goesTo"][number], PullRequest> => row.kind === "save" && (row.into?.landing.destination.id ?? PARENT) === saving);

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

  const own = rows.own?.kind === "starting_point" ? (
    <Row icon={<CircleDashedIcon className="size-4 text-muted-foreground" />} title={`${rows.own.name} isn't deployed`} detail="A starting point for branches">
      <Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO} params={params} className={buttonVariants({ size: "sm" })}>
        <GitBranchPlusIcon data-icon="inline-start" />New branch
      </Link>
    </Row>
  ) : rows.own?.kind === "staged" ? (
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
          <DropdownMenuItem variant="destructive" disabled={discarding || !groups.some((group) => group.canDiscard)} onClick={() => void discardAll()}>
            Discard all changes
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </Row>
  ) : rows.own?.kind === "attempt" ? <AttemptState environmentId={environmentId} deploymentId={rows.own.deploymentId} />
    : rows.own?.kind === "shutdown" ? <ShutdownState environmentId={environmentId} shutdown={rows.own.shutdown} />
    : rows.own?.kind === "waiting" ? <WaitingState saves={waiting} /> : null;
  // Saying where only when more than one Destination has something.
  const several = rows.parent.filter((row) => row.kind === "saved" || (row.kind === "save" && row.into)).length > 1;
  const parent = !review ? [] : rows.parent.map((row) => {
    if (row.kind === "update") return <UpdatesState key="update" review={review} environmentId={environmentId} />;
    if (row.kind === "saved") {
      return <SavedState key={`saved:${row.landing.destination.id}`} review={review} saved={row.saved} pullRequest={row.pullRequest} environmentId={environmentId} several={several} />;
    }
    const key = row.into?.landing.destination.id ?? PARENT;
    return (
      <Row key={`save:${key}`} icon={<GitBranchIcon className="size-4 text-muted-foreground" />}
        title={`${plural(row.into?.landing.rows.length ?? review.changes, "change")} to save`}
        detail={!row.into ? `into ${review.parent.name}`
          : `${several ? `into ${row.into.landing.destination.name} · ` : ""}go live when PR #${row.into.pullRequest.number} merges`}>
        <Button size="sm" variant="outline" onClick={() => setSaving(key)}>Save</Button>
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
    onDiscardAll: () => void discardAll(), discarding,
    onDiscardNode, onDiscardRow,
    noteFor: (group: CanvasEnvironmentChangeGroup, path: string) => {
      const note = landed.notes.find((candidate) => candidate.nodeId === group.nodeId && candidate.path === path);
      return note ? <LandedNote note={note} /> : null;
    },
    after: (
      <>
        <LandedRest notes={landed.rest} />
        {/* Saved to go live with pull requests: read-only here, never changes to deploy. */}
        {waiting.length ? <GoesLiveChanges title={waitingTitle(waiting)} saves={waiting}
          nameOf={(lineage, prEnvironmentId) => lineageName(lineage, prEnvironmentId ?? undefined)} /> : null}
      </>
    ),
  };

  return (
    <>
      {bar && slot ? createPortal(bar, slot) : null}
      {open ? <EnvironmentChangesReview {...reviewProps} /> : null}
      {saveInto && review ? saveInto.into
        ? <PrSaveSheet key={saving} review={review} branchId={environmentId} landing={saveInto.into.landing} pullRequest={saveInto.into.pullRequest} onClose={() => setSaving(null)} />
        : <SaveSheet key={saving} review={review} branchId={environmentId} onClose={() => setSaving(null)} /> : null}
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

const PARENT = "parent";

/** "3 changes go live with PR #142". */
const waitingTitle = (saves: ConditionalSaveRow[]) =>
  `${goLive(saves.reduce((n, save) => n + save.rows.length, 0))} with ${listNames(saves.map((save) => `PR #${save.prNumber}`))}`;

/** A PR Environment's changes saved for one Destination: they go live when its pull request merges. Details holds Undo. */
function SavedState({ review, saved, pullRequest, environmentId, several }: {
  review: BranchReviewView; saved: ConditionalSaveRow; pullRequest: PullRequest; environmentId: string; several: boolean;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { withdraw } = useConditionalSave({
    organizationSlug: params.organizationSlug, prEnvironmentId: environmentId, destinationEnvironmentId: saved.destinationEnvironmentId,
  });
  const [details, setDetails] = useState(false);
  const into = several ? ` · into ${review.environmentName(saved.destinationEnvironmentId)}` : "";
  return (
    <Row icon={<GitPullRequestIcon className="size-4 text-muted-foreground" />} title={goLive(saved.rows.length)} detail={`when PR #${pullRequest.number} merges${into}`}>
      <Button size="sm" variant="outline" onClick={() => setDetails(true)}>Details</Button>
      {details ? (
        <GoesLiveSheet title={`${goLive(saved.rows.length)} when PR #${pullRequest.number} merges`} saves={[saved]}
          nameOf={(lineage) => review.nameOf(lineage)} onClose={() => setDetails(false)}
          actions={<Button variant="outline" disabled={withdraw.isPending} onClick={() => withdraw.mutate(undefined, { onSuccess: () => setDetails(false) })}>Undo</Button>} />
      ) : null}
    </Row>
  );
}

/**
 * A shutdown: running, then Off with its settings kept (the next push starts it again, or Deploy now), or failed, when
 * Shut down runs again.
 */
function ShutdownState({ environmentId, shutdown }: { environmentId: string; shutdown: PrShutdown }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const document = useEnvironmentDocuments(params.organizationSlug).find((candidate) => candidate.id === environmentId);
  const { start, shutDown } = usePrEnvironmentOff({ organizationSlug: params.organizationSlug, environmentId, name: document?.name ?? params.environmentSlug });
  const icon = <PowerOffIcon className="size-4 text-muted-foreground" />;
  if (shutdown === "running") return <Row icon={icon} title="Shutting down" detail="Deploy once it's off">{null}</Row>;
  if (shutdown === "failed") {
    return (
      <Row icon={<PowerOffIcon className="size-4 text-destructive" />} title="Shutdown failed" detail="Some services may still run">
        <Button size="sm" variant="outline" disabled={shutDown.isPending} onClick={() => shutDown.mutate()}>Shut down</Button>
      </Row>
    );
  }
  return (
    <Row icon={icon} title="Off" detail="Starts again on the next push">
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
  const title = waitingTitle(saves);
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
      {unsettled || review.update.length === 0
        ? <Link to={ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO} params={params} className={buttonVariants({ size: "sm", variant: "outline" })}>Details</Link>
        : <Button size="sm" variant="outline" onClick={() => update(environmentId)}>Update</Button>}
    </Row>
  );
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
