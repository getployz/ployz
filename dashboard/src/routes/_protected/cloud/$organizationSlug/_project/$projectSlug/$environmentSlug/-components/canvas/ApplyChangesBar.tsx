"use client";

import { useEffect, useRef, useState } from "react";
import { Button } from "#/components/ui/button";
import type {
  CanvasEnvironmentChangeGroup,
} from "#/modules/environment-design/canvas-environment-change-state";
import { EnvironmentChangesReview } from "./EnvironmentChangesReview";

type ApplyChangesBarProps = {
  groups: CanvasEnvironmentChangeGroup[];
  totalChanges: number;
  canDeploy?: boolean;
  commitMessage: string;
  canSaveWithoutDeploying: boolean;
  onCommitMessageChange: (value: string) => void;
  onDeploy: () => void;
  onSaveWithoutDeploying: () => void;
  onDiscardAll: () => Promise<boolean>;
  onDiscardNode: (group: CanvasEnvironmentChangeGroup) => void;
  onDiscardRow: (group: CanvasEnvironmentChangeGroup, path: string) => void;
};

export function ApplyChangesBar({
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
}: ApplyChangesBarProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  const hasChanges = totalChanges > 0 || canSaveWithoutDeploying;

  useEffect(() => {
    if (wasOpen.current && !open) {
      (triggerRef.current ?? workspaceRef.current)?.focus({ preventScroll: true });
    }
    wasOpen.current = open;
  }, [open]);

  if (!hasChanges && !open) return null;

  return (
    <>
      <div className="canvas-change-controls">
        <span className="text-sm text-muted-foreground tabular-nums">
          {totalChanges > 0
            ? `${totalChanges} ${totalChanges === 1 ? "change" : "changes"}`
            : canSaveWithoutDeploying ? "Unpublished changes" : "No changes"}
        </span>
        <Button
          ref={triggerRef}
          variant="outline"
          aria-expanded={open}
          onClick={() => {
            workspaceRef.current = triggerRef.current?.closest<HTMLElement>(".environment-canvas-scene") ?? null;
            setOpen(!open);
          }}
        >
          Review changes
        </Button>
      </div>
      {open ? <EnvironmentChangesReview
        groups={groups}
        totalChanges={totalChanges}
        canDeploy={(canDeploy ?? true) && totalChanges > 0}
        canSave={canSaveWithoutDeploying}
        commitMessage={commitMessage}
        onClose={() => setOpen(false)}
        onCommitMessageChange={onCommitMessageChange}
        onDeploy={() => { setOpen(false); onDeploy(); }}
        onSave={() => { setOpen(false); onSaveWithoutDeploying(); }}
        onDiscardAll={async () => { if (await onDiscardAll()) setOpen(false); }}
        onDiscardNode={onDiscardNode}
        onDiscardRow={onDiscardRow}
      /> : null}
    </>
  );
}
