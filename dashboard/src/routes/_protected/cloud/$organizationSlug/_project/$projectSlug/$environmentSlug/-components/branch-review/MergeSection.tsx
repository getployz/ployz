import { ItemGroup } from "#/components/ui/item";
import { presentRow } from "#/modules/branches/branch-review";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";

/**
 * What would merge into the Destination, change by change: the Branch's Working State against the Destination's, over the
 * base. Read-only; #1159 adds ticks, value choices, Then close and Merge.
 */
export function MergeSection({ review }: { review: BranchReviewView }) {
  const destination = review.parent.name;
  return (
    <ReviewSection title={`Merge into ${destination}`} count={review.merge.length}>
      {review.merge.length ? (
        <ItemGroup className="gap-1">
          {review.merge.map((row) => (
            <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)}
              conflict={row.role === "move" && row.conflict ? destination : undefined} />
          ))}
        </ItemGroup>
      ) : <p className="text-sm text-muted-foreground">Nothing here that {destination} doesn't have.</p>}
    </ReviewSection>
  );
}
