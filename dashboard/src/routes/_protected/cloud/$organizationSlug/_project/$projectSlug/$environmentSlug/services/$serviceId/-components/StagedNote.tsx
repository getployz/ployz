/**
 * What the next Deploy changes on a row, in the staged-intent colour the Details list uses: the deployed value it
 * replaces. Discarding it is the changes dialog's job.
 */
export function StagedNote({ before }: { before: string }) {
  return <p className="truncate text-sm text-changed-deep">Was {before}</p>;
}
