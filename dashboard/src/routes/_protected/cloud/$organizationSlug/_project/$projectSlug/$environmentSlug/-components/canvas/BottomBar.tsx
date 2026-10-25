"use client";

import { createContext, useContext, useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { toast } from "sonner";
import { Link, useParams } from "@tanstack/react-router";
import { MoreVerticalIcon, XIcon } from "lucide-react";
import { AgentDock } from "#/components/agent-sidebar/agent-dock";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { buttonVariants } from "#/components/ui/button-variants";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "#/components/ui/tooltip";
import type { DeploymentSummary, Included } from "@ployz/sdk";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { deploymentLabel, deploymentLight, deploymentStatusLabel, deploysLabel, uploadLabel, type ChangeGroup } from "#/modules/config-store/store-deployments";
import { listNames, plural } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { deploymentDestination } from "../deployments/run-search";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";
import type { Busy } from "#/modules/config-store/store-pending";
import { AddServersDialog } from "#/routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/add-servers-dialog";
import type { DeployChoice } from "./useStoreChangeActions";

/**
 * Where the bottom bar renders while the agent is closed: the scene, outside the canvas that turns inert under a panel.
 * The canvas owns the change state, so it portals the bar here, or into the open agent's dock.
 */
export const BottomBarSlot = createContext<HTMLElement | null>(null);

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
  /** What the bar counts as changes: edits not yet saved. Defaults to `totalChanges`. */
  stagedChanges?: number;
  /** The latest Save when it is all there is to deploy: part of it is neither applied nor shipped by a Deploy in flight, which may ship the rest. */
  undeployedSave?: number | null;
  /** The latest Save when every Deploy of it ran nothing: none of its changes reached a Server. */
  ranNothingSave?: number | null;
  canSave: boolean;
  /** Deploys, with the user's Deploy message (blank for none). */
  onDeploy: (message: string, choice: DeployChoice) => void;
  /** Save only: saves without deploying; resolves true once saved. */
  onSave: (message: string) => Promise<boolean>;
  onDiscardAll: () => void;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  /** The Environment's in-flight Deployments, newest first, whoever admitted them. */
  active: DeploymentSummary[];
  /**
   * Its newest numbered Deployment, whatever page it fell on; null before the first Deploy, undefined until it loads.
   * Ended, it shows Failed until dismissed, or how it ended if seen in flight here. Holds that never ran take no
   * number, so a burst of them never hides it.
   */
  latest?: DeploymentSummary | null;
  /** A running Deploy's storage step, in words, while it moves or copies a Volume before its Services. */
  storage?: (deploymentId: string) => string | null;
  /** Details' notes: where changes came from, and the Parent's deployed values. */
  notes: Pick<ReviewProps, "noteFor" | "neverSyncFor" | "originFor" | "after">;
  /** Details' Included list: the sources Syncs included here, with Include newer changes and Remove. */
  proposals?: Pick<ReviewProps, "included" | "onRemoveIncluded" | "onIncludeNewer">;
  /** The Organization has no Server to deploy to: Deploy becomes Add a server; Save only still works. */
  noServers?: boolean;
  /** The Environment is Protected and you can't protect it: Deploy asks an owner or admin first. */
  asks?: boolean;
  /** Secrets without a value: the next Deploy leaves them out. */
  notSet?: number;
  /** A write the Store hasn't answered for yet, in words (Discarding…). */
  busy?: ReadonlySet<Busy>;
  discarding?: ReadonlySet<string>;
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
  stagedChanges = totalChanges,
  undeployedSave = null,
  ranNothingSave = null,
  canSave,
  onDeploy,
  onSave,
  onDiscardAll,
  onDiscardNode,
  onDiscardRow,
  active: newestFirst,
  latest: newest,
  storage = () => null,
  notes,
  proposals,
  noServers = false,
  asks = false,
  notSet = 0,
  busy = new Set(),
  discarding,
}: BottomBarProps) {
  const slot = useContext(BottomBarSlot);
  const dock = useContext(AgentDock);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  // Oldest first: the oldest holds, or is next for, the Environment's one run.
  const active = [...newestFirst].reverse();
  const hasChanges = totalChanges > 0 || canSave;
  // One Save at a time: a second sends the same version and comes back stale.
  const saveable = canSave && !busy.has("saving") && !busy.has("deploying");
  // A double click admits one Deploy: Deploy waits until the Store answers.
  const admitting = busy.has("saving") || busy.has("deploying");
  // A rollback to what the Servers already run still deploys: it saves, and its Deployment ends up to date.
  const deployable = hasChanges && !noServers;
  const shown = hasChanges ? undefined : active[0];
  // Seen in flight here: how a Deployment ends shows only when this bar watched it run. A failure shows from any tab, so a reload keeps it.
  const watched = useRef(new Set<string>());
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(new Set());
  const dismissedKey = `bottom-bar-dismissed:${params.organizationSlug}/${params.projectSlug}/${params.environmentSlug}`;
  const dismissedFailure = useSyncExternalStore(subscribeStorage, () => localStorage.getItem(dismissedKey), () => null);
  useEffect(() => {
    const seen = watched.current;
    for (const deployment of active) seen.add(deployment.id);
    for (const id of seen) if (seen.size > WATCHED_LIMIT) seen.delete(id);
  }, [active]);
  const latest = active.length > 0 || newest?.in_flight ? undefined : newest ?? undefined;
  const showsLatest = latest !== undefined && (latest.status === "failed" ? latest.id !== dismissedFailure : watched.current.has(latest.id));
  const outcome = shown || !showsLatest || dismissed.has(latest.id) ? undefined : latest;
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

  function deploy(noPrune = false) {
    if (admitting) return;
    setOpen(false);
    onDeploy(message, { noPrune });
    setMessage("");
  }

  // A message edited while the save is in flight is the next one: keep it.
  function save() {
    void onSave(message).then((saved) => {
      if (!saved) return;
      setMessage((now) => now === message ? "" : now);
      toast.success("Saved, not deployed");
    });
  }

  // Alt turns Deploy into Save only, even with nothing left to save: an Alt-click never deploys.
  const savesOnly = useAltHeld();

  // New Services and Volumes go for good with a Discard: name them and ask first.
  const created = groups.filter((group) => group.lifecycle === "create" && group.canDiscard).map((group) => group.nodeName);

  function discardAll() {
    if (created.length > 0) setConfirmingDiscard(true);
    else onDiscardAll();
  }

  // The dialog stays up through its exit animation: a second click there neither reopens it nor discards again.
  function confirmDiscard() {
    if (!confirmingDiscard) return;
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

  const subject = toDeploy(stagedChanges, undeployedSave, ranNothingSave !== null && ranNothingSave === undeployedSave, active);
  // With only a Save left to deploy, Discard all returns to that Save: nothing would change.
  const nothingToDiscard = subject.kind === "save";
  const counted = countedWords(subject);
  const staged = hasChanges ? (
    <Row staged title={busy.has("discarding") ? "Discarding…" : ((counted ?? "Changes to save") + (notSet > 0 ? ` · ${notSet} not set` : ""))}
      shortTitle={counted ?? "To save"} detail={null}>
      <Button ref={triggerRef} variant="outline" aria-expanded={open} onClick={openReview}>Details</Button>
      {/* Deploying behind a running or queued attempt queues. */}
      {noServers ? (
        <AddServersDialog organizationSlug={params.organizationSlug} label="Add servers" variant="intent" />
      ) : <Tooltip>
        <TooltipTrigger render={<Button variant="intent" disabled={savesOnly ? !saveable : !deployable || admitting} aria-keyshortcuts="Shift+Enter"
          onClick={(event) => { if (!savesOnly && !event.altKey) deploy(); else if (saveable) save(); }} />}>
          {admitting ? <Spinner data-icon="inline-start" /> : null}
          {busy.has("deploying") ? "Deploying…" : busy.has("saving") ? "Saving…" : savesOnly ? "Save only" : asks ? "Request deploy" : active.length > 0 ? "Deploy next" : "Deploy"}
        </TooltipTrigger>
        <TooltipContent>⇧+Enter</TooltipContent>
      </Tooltip>}
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon" variant="ghost" aria-label="More change actions" title="More change actions" />}>
          <MoreVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="top" className="w-auto">
          <DropdownMenuItem disabled={!saveable} onClick={save}>
            {busy.has("saving") ? <><Spinner />Saving…</> : "Save only"}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={!deployable || admitting} onClick={() => deploy(true)}>
            Deploy, keep other services
          </DropdownMenuItem>
          {nothingToDiscard ? null : (
            <DropdownMenuItem variant="destructive" disabled={busy.has("discarding") || !groups.some((group) => group.canDiscard)} onClick={discardAll}>
              Discard all changes
            </DropdownMenuItem>
          )}
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
    environment: params.environmentSlug, groups, totalChanges, canDeploy: deployable, asks, canSave: saveable, ranNothingSave,
    onClose: () => setOpen(false), onDeploy: () => deploy(), message, onMessageChange: setMessage, busy, discarding,
    onSave: save,
    onDiscardAll: nothingToDiscard ? undefined : discardAll,
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
        description={`${listNames(created)} ${created.length === 1 ? "is" : "are"} new and will be deleted. Other changes return to the latest saved version.`}
        actionLabel="Discard all" variant="destructive" onConfirm={confirmDiscard} />
    </>
  );
}

/**
 * What the changes to deploy are: edits not yet saved, else the latest Save part of which is left to deploy, with the
 * oldest Deployment in flight that ships it, if any (one that takes the Environment off the Servers ships none);
 * `nothing` when neither, as when only what a Sync included is left to save.
 */
type ToDeploy =
  | { kind: "changes"; count: number }
  | { kind: "save"; save: number; ranNothing: boolean; shipping: DeploymentSummary | undefined }
  | { kind: "nothing" };

function toDeploy(stagedChanges: number, save: number | null, ranNothing: boolean, active: readonly DeploymentSummary[]): ToDeploy {
  if (stagedChanges > 0) return { kind: "changes", count: stagedChanges };
  if (save === null) return { kind: "nothing" };
  return { kind: "save", save, ranNothing, shipping: active.find((deployment) => !deployment.remove && deployment.saved === save) };
}

/** The row's words for its changes. A Save is never "not running": beside Service cards that reads as an outage. */
function countedWords(subject: ToDeploy) {
  switch (subject.kind) {
    case "changes":
      return plural(subject.count, "change");
    case "save":
      if (subject.shipping) return `${deploymentStatusLabel(subject.shipping)} · Save #${subject.save}`;
      return subject.ranNothing ? `Save #${subject.save} · its deploy ran nothing` : `Save #${subject.save} not deployed`;
    case "nothing":
      return null;
  }
}

/** Whether Alt is held, so a button can say what an Alt-click does. */
function useAltHeld() {
  const [held, setHeld] = useState(false);
  useEffect(() => {
    const track = (event: KeyboardEvent) => setHeld(event.altKey);
    const release = () => setHeld(false);
    window.addEventListener("keydown", track);
    window.addEventListener("keyup", track);
    window.addEventListener("blur", release);
    return () => {
      window.removeEventListener("keydown", track);
      window.removeEventListener("keyup", track);
      window.removeEventListener("blur", release);
    };
  }, []);
  return held;
}

function AttemptState({ deployment, storage, onDismiss }: { deployment: DeploymentSummary; storage: string | null; onDismiss?: () => void }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const deploys = deploysLabel(deployment);
  const upload = uploadLabel(deployment);
  const detail = storage ?? (deploys && upload ? `${deploys} · ${upload}` : deploys);
  return (
    <Row icon={<DeploymentStatusIcon status={deploymentLight(deployment)} />}
      title={`${deploymentStatusLabel(deployment)} · Deployment ${deploymentLabel(deployment)}`} detail={detail}>
      <Link {...deploymentDestination(params, deployment.id)} className={buttonVariants({ variant: "outline" })}>
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
