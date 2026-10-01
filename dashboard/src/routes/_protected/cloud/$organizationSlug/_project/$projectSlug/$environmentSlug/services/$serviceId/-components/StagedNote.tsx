import { Button } from "#/components/ui/button";

/**
 * What the next Deploy changes on a row, in the staged-intent colour the Details list uses: the deployed value it
 * replaces, and Undo, which drops that one change.
 */
export function StagedNote({ before, onUndo }: { before: string; onUndo: () => void }) {
  return (
    <p className="flex min-w-0 items-center gap-1 text-sm text-changed-deep">
      <span className="truncate">Was {before}</span>
      <Button type="button" variant="link" size="xs" onClick={onUndo}>Undo</Button>
    </p>
  );
}
