"use client";

import { useContext, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { MoreVerticalIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Kbd } from "#/components/ui/kbd";
import { useIsMobile } from "#/hooks/use-mobile";
import type {
  CanvasEnvironmentChangeGroup,
} from "#/modules/environment-design/canvas-environment-change-state";
import { ApplyZoneSlot } from "../DeployBar";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";

type ApplyZoneProps = {
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

/** The deploy bar's apply zone: Apply N changes · Details · Deploy ⇧+Enter · ⋮ Discard. Rendered into the bar's slot. */
export function ApplyZone({
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
}: ApplyZoneProps) {
  const slot = useContext(ApplyZoneSlot);
  const isMobile = useIsMobile();
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  const hasChanges = totalChanges > 0 || canSaveWithoutDeploying;
  const deployable = canDeploy && totalChanges > 0;
  const count = totalChanges > 0 ? `Apply ${totalChanges}` : "Unpublished";

  function deploy() {
    setOpen(false);
    onDeploy();
  }

  function openDetails() {
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

  return (
    <>
      {hasChanges && slot ? createPortal(
        <div className="apply-zone">
          {/* On a phone the count is the Details trigger, so the bar fits 375px. */}
          {isMobile ? (
            <Button ref={triggerRef} size="sm" variant="outline" aria-expanded={open} aria-label={`Details, ${count}`} onClick={openDetails}>{count}</Button>
          ) : <>
            <span className="px-1.5 text-xs font-medium text-changed-deep tabular-nums">{count} {totalChanges === 1 ? "change" : "changes"}</span>
            <Button ref={triggerRef} size="sm" variant="outline" aria-expanded={open} onClick={openDetails}>Details</Button>
          </>}
          <Button size="sm" disabled={!deployable} aria-keyshortcuts="Shift+Enter" onClick={deploy}>
            Deploy{isMobile ? null : <Kbd>⇧+Enter</Kbd>}
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
        </div>,
        slot,
      ) : null}
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
