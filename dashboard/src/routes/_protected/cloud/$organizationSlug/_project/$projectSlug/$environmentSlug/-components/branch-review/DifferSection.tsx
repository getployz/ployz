import { ItemDescription, ItemGroup } from "#/components/ui/item";
import { DIFFER_REASONS, presentRow, type ChangeRow } from "#/modules/branches/branch-review";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";

/** Settings each Environment keeps as its own, so they never move, each with its reason. */
export function DifferSection({ review }: { review: BranchReviewView }) {
  return (
    <ReviewSection title="Meant to differ">
      {review.differ.length ? <ItemGroup className="gap-1">
        {review.differ.map((row) => (
          <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)}
            description={<ItemDescription>{reason(row, review)}</ItemDescription>} />
        ))}
      </ItemGroup> : <p className="text-sm text-muted-foreground">Nothing here is meant to differ.</p>}
    </ReviewSection>
  );
}

/** Replicas in numbers, as "Runs 1 replica · staging runs 3"; any other row by its reason. */
function reason(row: ChangeRow, review: BranchReviewView) {
  if (row.role !== "differ") return null;
  const { after, before } = presentRow(row, review.nameOf);
  if (row.key.endsWith(":replicas") && after && before) {
    return `Runs ${after} ${after === "1" ? "replica" : "replicas"} · ${review.parent.name} runs ${before}`;
  }
  return DIFFER_REASONS[row.why];
}
