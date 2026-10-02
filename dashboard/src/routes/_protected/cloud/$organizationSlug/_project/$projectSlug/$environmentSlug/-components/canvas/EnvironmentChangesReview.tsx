import type { ReactNode } from "react";
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "#/components/ui/dialog";
import { Button } from "#/components/ui/button";
import { InputGroup, InputGroupInput } from "#/components/ui/input-group";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { ApplyChangeGroupCard } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/ApplyChangeGroupCard";

export type EnvironmentChangesReviewProps = {
  groups: ChangeGroup[];
  totalChanges: number;
  canDeploy: boolean;
  /** Working State differs from Saved State: Publish saves it without deploying. */
  canPublish: boolean;
  onPublish: () => void;
  onDiscardAll: () => void;
  onClose: () => void;
  onDeploy: () => void;
  /** What the next Deploy ships, in the user's words; shown on its Deployment. */
  message: string;
  onMessageChange: (message: string) => void;
  /** A Deploy is being admitted: Deploy waits. */
  admitting?: boolean;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  /**
   * A note beside a change (or with the group's `discardPath`, beside its node): where it came from, or a merged pull
   * request's or the Parent's value with Use.
   */
  noteFor?: (group: ChangeGroup, path: string) => ReactNode;
  /** Never sync for a change that arrived from another Environment: marks it and discards it. */
  neverSyncFor?: (group: ChangeGroup, path: string) => (() => void) | undefined;
  /** Lists after the changes: merged pull requests' and the Parent's values no change shows. */
  after?: ReactNode;
};

export function EnvironmentChangesReview(props: EnvironmentChangesReviewProps) {
  const { canPublish, totalChanges, onClose, after } = props;
  const staged = canPublish || totalChanges > 0;
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent padding="none" className="flex max-h-[85dvh] flex-col overflow-hidden sm:max-w-3xl">
      <div className="shrink-0 border-b px-6 py-4 pr-12">
        <div>
          <DialogTitle>Environment changes</DialogTitle>
          <DialogDescription className="mt-2">
            {canPublish ? "Not yet published" : totalChanges > 0 ? "Published · not yet deployed" : "Nothing staged here"}
          </DialogDescription>
        </div>
      </div>
      {staged ? <StagedChanges {...props} /> : <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto px-6 py-6">{after}</div>}
      </DialogContent>
    </Dialog>
  );
}

/** The staged-changes review itself: the Deploy message, the changes, Discard, Publish and Deploy. */
function StagedChanges({
  groups,
  totalChanges,
  canDeploy,
  canPublish,
  onPublish,
  onDiscardAll,
  onClose,
  onDeploy,
  message,
  onMessageChange,
  admitting = false,
  onDiscardNode,
  onDiscardRow,
  noteFor,
  neverSyncFor,
  after,
}: EnvironmentChangesReviewProps) {
  return (
    <>
      {canDeploy ? (
        <div className="shrink-0 border-b px-6 py-3">
          <InputGroup>
            <InputGroupInput aria-label="Deploy message" placeholder="Deploy message (optional)" maxLength={500} value={message}
              onChange={(event) => onMessageChange(event.target.value)} />
          </InputGroup>
        </div>
      ) : null}
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-6">
          <div className="flex flex-col gap-3">
            {groups.map((group) => (
              <ApplyChangeGroupCard
                key={`${group.nodeType}:${group.nodeId}`}
                group={group}
                totalChanges={totalChanges}
                visibleGroupCount={groups.length}
                onCloseDialog={onClose}
                onDiscardNode={() => onDiscardNode(group)}
                onDiscardRow={(_, path) => onDiscardRow(group, path)}
                noteFor={noteFor && ((path) => noteFor(group, path))}
                neverSyncFor={neverSyncFor && ((path) => neverSyncFor(group, path))}
              />
            ))}
            {after}
          </div>
        </div>

      <div className="flex shrink-0 flex-wrap items-center justify-end gap-2 border-t px-6 py-4">
        {groups.some(group => group.canDiscard) ? <Button variant="ghost" className="mr-auto" onClick={onDiscardAll}>Discard all changes</Button> : null}
        <Button variant="outline" disabled={!canPublish} onClick={onPublish}>Publish</Button>
        {canDeploy ? <Button disabled={admitting} onClick={onDeploy}>Deploy changes</Button> : null}
      </div>
    </>
  );
}
