"use client";

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useParams } from "@tanstack/react-router";
import { GitPullRequestIcon, MoreVerticalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import type { DeploymentSummary } from "@ployz/sdk";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { deploymentStatusIcons, deploymentStatusLabels, targetsLabel, uploadLabel } from "#/modules/config-store/store-deployments";
import { useDeploymentAttempt, useEnvironmentDeployments } from "#/modules/deployments/deployment.collection";
import { activeStep, deploymentStatusLabel } from "#/modules/deployments/deployment-view";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { listNames, plural } from "#/modules/branches/branch-plan";
import { rowLineage } from "#/modules/branches/branch-review";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useWaitingSaves } from "#/modules/pr-environments/conditional-save.collection";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { LandedNote, LandedRest, useLandedNotes } from "../branch-review/landed-notes";
import { GoesLiveChanges, GoesLiveSheet } from "../branch-review/SaveSheet";
import { goLive } from "#/modules/pr-environments/pr-check";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "../deployment-page";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_INDEX_ROUTE_TO } from "../environment-route-paths";
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
  /** None when Deploys carry no message (the Config Store's). */
  onCommitMessageChange?: (value: string) => void;
  onDeploy: () => void;
  onSaveWithoutDeploying: () => void;
  onDiscardAll: () => Promise<boolean>;
  onDiscardNode: (group: CanvasEnvironmentChangeGroup) => void;
  onDiscardRow: (group: CanvasEnvironmentChangeGroup, path: string) => void;
  /** Over the Config Store: its in-flight Deployments, newest first, whoever admitted them. Null reads the legacy attempts. */
  storeActive?: DeploymentSummary[] | null;
};

/**
 * The bottom bar holds this Environment's own changes and nothing else, in one row like Railway's: changes to deploy
 * ("Apply 3 changes · Details · Deploy · ⋮"), else a running or queued attempt whose page isn't open, else changes that
 * go live here with a pull request. What moves between Environments, Save and Update, is the Branch button's, at the
 * canvas's top right; it lands in a bottom bar as changes to deploy.
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
  storeActive = null,
}: BottomBarProps) {
  const slot = useContext(BottomBarSlot);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const viewedId = useCanvasInspectorSelection().deploymentId;
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Newest first: the oldest active attempt holds, or is next for, the Environment execution slot.
  const legacyActive = useEnvironmentDeployments(params.organizationSlug, environmentId)
    .filter(({ deployment }) => isActiveDeployment(deployment.status)).map(({ deployment }) => deployment).reverse();
  const active = storeActive ? [...storeActive].reverse() : legacyActive;
  const hasChanges = totalChanges > 0 || canSaveWithoutDeploying;
  const deployable = hasChanges && canDeploy && totalChanges > 0;
  const shown = hasChanges ? undefined : active.find((deployment) => deployment.id !== viewedId);
  const shownStore = storeActive?.find((deployment) => deployment.id === shown?.id);
  // Saved to go live with a pull request, not changes to deploy here: the bar's last state.
  const waiting = useWaitingSaves(params.organizationSlug, environmentId);
  const landed = useLandedNotes(environmentId, groups);
  const lineageName = useLineageNames(params.organizationSlug);

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
    <Row staged title={totalChanges > 0 ? `Apply ${plural(totalChanges, "change")}` : "Unpublished changes"} detail={null}>
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
  ) : shownStore ? <StoreAttemptState deployment={shownStore} />
    : shown ? <AttemptState environmentId={environmentId} deploymentId={shown.id} />
    : waiting.length ? <WaitingState saves={waiting} /> : null;
  const bar = row ? <div role="group" aria-label="Bottom bar" className="bottom-bar">{row}</div> : null;

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
    </>
  );
}

/** "3 changes go live with PR #142". */
const waitingTitle = (saves: ConditionalSaveRow[]) =>
  `${goLive(saves.reduce((n, save) => n + save.rows.length, 0))} with ${listNames(saves.map((save) => `PR #${save.prNumber}`))}`;

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
      <Button variant="outline" onClick={() => setDetails(true)}>Details</Button>
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
        className={buttonVariants({ variant: "outline" })}
      >
        Logs
      </Link>
    </Row>
  );
}

/** An in-flight Store Deployment, the CLI's or this dashboard's: its status, what it ships, and Logs to open its page. */
function StoreAttemptState({ deployment }: { deployment: DeploymentSummary }) {
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
