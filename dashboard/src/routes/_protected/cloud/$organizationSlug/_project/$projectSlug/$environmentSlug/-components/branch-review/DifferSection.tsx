import { ItemDescription, ItemGroup } from "#/components/ui/item";
import { DIFFER_REASONS, presentRow } from "#/modules/branches/branch-review";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";

/** Settings each Environment keeps as its own, so they never move, each with its reason. */
export function DifferSection({ review }: { review: BranchReviewView }) {
  return (
    <ReviewSection title="Meant to differ" help={review.differ.length ? "These never move in a merge or an update." : undefined}>
      {review.differ.length ? <ItemGroup className="gap-1">
        {review.differ.map((row) => (
          <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)}
            description={<ItemDescription>{row.role === "differ" ? DIFFER_REASONS[row.why] : null}</ItemDescription>} />
        ))}
      </ItemGroup> : <p className="text-sm text-muted-foreground">Nothing here is meant to differ.</p>}
    </ReviewSection>
  );
}
