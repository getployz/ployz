import { CheckIcon, PencilIcon } from "lucide-react";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogTitle,
} from "#/components/ui/dialog";
import { InputGroup, InputGroupInput } from "#/components/ui/input-group";
import { Badge } from "#/components/ui/badge";
import type {
  CanvasEnvironmentChangeGroup,
  CanvasEnvironmentChangeSlice,
  CanvasDeploymentEvidence,
} from "#/modules/environment-design/canvas-environment-change-state";
import { ApplyChangeGroupCard } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/ApplyChangeGroupCard";

export function ApplyChangesDialog({
  groups,
  slices,
  totalChanges,
  canDeploy,
  deploymentEvidence,
  commitMessage,
  open,
  onCommitMessageChange,
  onDeploy,
  onDiscardNode,
  onDiscardRow,
  onOpenChange,
}: {
  groups: CanvasEnvironmentChangeGroup[];
  slices?: Record<"unsaved" | "pending" | "drift", CanvasEnvironmentChangeSlice>;
  totalChanges: number;
  canDeploy: boolean;
  deploymentEvidence?: CanvasDeploymentEvidence | null;
  commitMessage: string;
  open: boolean;
  onCommitMessageChange: (value: string) => void;
  onDeploy: () => void;
  onDiscardNode: (group: CanvasEnvironmentChangeGroup) => void;
  onDiscardRow: (group: CanvasEnvironmentChangeGroup, path: string) => void;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-h-[calc(100dvh-2rem)] max-w-[calc(100%-2rem)] grid-rows-[auto_auto_minmax(0,1fr)_auto] overflow-hidden sm:max-w-4xl"
        padding="none"
      >
        <div className="px-6 py-4 pr-14">
          <DialogTitle>Environment changes</DialogTitle>
          {slices ? <div className="mt-2 flex flex-wrap gap-2">
            <Badge variant="changed">{slices.unsaved.totalCount} unsaved</Badge>
            <Badge variant="secondary">{slices.pending.totalCount} pending</Badge>
            <Badge variant="outline">{slices.drift.totalCount} drift</Badge>
          </div> : null}
          {deploymentEvidence ? (
            <div className="mt-2">
              <Badge variant="secondary">
                Deployment {deploymentEvidence.status}
              </Badge>
            </div>
          ) : null}
        </div>

        <div className="flex items-center gap-3 border-y px-6 py-3">
          <Avatar>
            <AvatarFallback>
              <PencilIcon />
            </AvatarFallback>
          </Avatar>
          <InputGroup>
            <InputGroupInput
              aria-label="Commit message"
              placeholder="Commit message (optional)"
              value={commitMessage}
              onChange={(event) => onCommitMessageChange(event.target.value)}
            />
          </InputGroup>
        </div>

        <div className="overflow-y-auto px-6 py-6">
          <div className="flex flex-col gap-3">
            {groups.map((group) => (
              <ApplyChangeGroupCard
                key={`${group.slice ?? "change"}:${group.nodeType}:${group.nodeId}`}
                group={group}
                totalChanges={totalChanges}
                visibleGroupCount={groups.length}
                onCloseDialog={() => onOpenChange(false)}
                onDiscardNode={(candidate) => {
                  // SAFETY: the card callbacks the same `group` object we passed in, which is CanvasEnvironmentChangeGroup.
                  onDiscardNode(candidate as CanvasEnvironmentChangeGroup);
                }}
                onDiscardRow={(candidate, path) => {
                  // SAFETY: the card callbacks the same `group` object we passed in, which is CanvasEnvironmentChangeGroup.
                  onDiscardRow(
                    candidate as CanvasEnvironmentChangeGroup,
                    path,
                  );
                }}
              />
            ))}
          </div>
        </div>

        {canDeploy ? <DialogFooter className="m-0 px-6 py-4">
          <Button onClick={onDeploy}>
              <CheckIcon data-icon="inline-start" />
            Deploy changes
          </Button>
        </DialogFooter> : null}
      </DialogContent>
    </Dialog>
  );
}
