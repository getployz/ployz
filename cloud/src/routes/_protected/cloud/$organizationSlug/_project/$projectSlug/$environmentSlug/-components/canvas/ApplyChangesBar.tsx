"use client";

import { useState } from "react";
import { Badge } from "#/components/ui/badge";
import type {
  CanvasEnvironmentChangeGroup,
  CanvasEnvironmentChangeSlice,
  CanvasDeploymentEvidence,
  CanvasDiscardAllPlan,
} from "#/modules/environment-design/canvas-environment-change-state";
import { ApplyChangesDialog } from "./ApplyChangesDialog";
import { ApplyChangesToolbar } from "./ApplyChangesToolbar";

type ApplyChangesBarProps = {
  groups: CanvasEnvironmentChangeGroup[];
  slices?: Record<
    "unsaved" | "pending" | "drift",
    CanvasEnvironmentChangeSlice
  >;
  totalChanges: number;
  discardAllPlan: CanvasDiscardAllPlan;
  canDeploy?: boolean;
  deploymentEvidence?: CanvasDeploymentEvidence | null;
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
  slices,
  totalChanges,
  discardAllPlan,
  canDeploy,
  deploymentEvidence,
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
  const canDiscardAll = discardAllPlan.nodes.length > 0;
  if (totalChanges <= 0) {
    return deploymentEvidence ? (
      <Badge className={className} variant="secondary">
        Deployment {deploymentEvidence.status}
      </Badge>
    ) : null;
  }

  return (
    <>
      <ApplyChangesToolbar
        className={className}
        canDiscardAll={canDiscardAll}
        canSaveWithoutDeploying={canSaveWithoutDeploying}
        canDeploy={canDeploy ?? true}
        deploymentEvidence={deploymentEvidence}
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

      <ApplyChangesDialog
        open={open}
        groups={groups}
        slices={slices}
        totalChanges={totalChanges}
        canDeploy={canDeploy ?? true}
        deploymentEvidence={deploymentEvidence}
        commitMessage={commitMessage}
        onOpenChange={setOpen}
        onCommitMessageChange={onCommitMessageChange}
        onDeploy={onDeploy}
        onDiscardNode={onDiscardNode}
        onDiscardRow={onDiscardRow}
      />
    </>
  );
}
