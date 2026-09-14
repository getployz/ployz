"use client";

import { useState } from "react";
import { cn } from "#/lib/utils";
import type {
  CanvasEnvironmentChangeGroup,
} from "#/modules/environment-design/canvas-environment-change-state";
import { ApplyChangesDialog } from "./ApplyChangesDialog";
import { ApplyChangesToolbar } from "./ApplyChangesToolbar";

type ApplyChangesBarProps = {
  groups: CanvasEnvironmentChangeGroup[];
  totalChanges: number;
  canDeploy?: boolean;
  commitMessage: string;
  canSaveWithoutDeploying: boolean;
  className?: string;
  onCommitMessageChange: (value: string) => void;
  onDeploy: () => void;
  onSaveWithoutDeploying: () => void;
  onDiscardAll: () => void;
  onDiscardNode: (group: CanvasEnvironmentChangeGroup) => void;
  onDiscardRow: (group: CanvasEnvironmentChangeGroup, path: string) => void;
};

export function ApplyChangesBar({
  groups,
  totalChanges,
  canDeploy,
  commitMessage,
  canSaveWithoutDeploying,
  className,
  onCommitMessageChange,
  onDeploy,
  onSaveWithoutDeploying,
  onDiscardAll,
  onDiscardNode,
  onDiscardRow,
}: ApplyChangesBarProps) {
  const [open, setOpen] = useState(false);
  if (totalChanges <= 0) return null;

  return (
    <>
      <div className={cn("apply-changes-surface pointer-events-auto rounded-xl border bg-background p-2 shadow-md", className)}>
        <ApplyChangesToolbar
          canDiscardAll={groups.some(group => group.canDiscard)}
          canSaveWithoutDeploying={canSaveWithoutDeploying}
          canDeploy={canDeploy ?? true}
          totalChanges={totalChanges}
          onOpenDetails={() => setOpen(true)}
          onDeploy={() => {
            setOpen(false);
            onDeploy();
          }}
          onSaveWithoutDeploying={() => {
            setOpen(false);
            onSaveWithoutDeploying();
          }}
          onDiscardAll={() => {
            setOpen(false);
            onDiscardAll();
          }}
        />
      </div>

      <ApplyChangesDialog
        open={open && totalChanges > 0}
        groups={groups}
        totalChanges={totalChanges}
        canDeploy={canDeploy ?? true}
        commitMessage={commitMessage}
        onOpenChange={setOpen}
        onCommitMessageChange={onCommitMessageChange}
        onDeploy={() => {
          setOpen(false);
          onDeploy();
        }}
        onDiscardNode={onDiscardNode}
        onDiscardRow={onDiscardRow}
      />
    </>
  );
}
