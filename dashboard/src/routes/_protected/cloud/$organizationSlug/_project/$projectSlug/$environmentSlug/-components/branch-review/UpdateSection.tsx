import { ItemDescription, ItemGroup } from "#/components/ui/item";
import { RelativeTime } from "#/components/relative-time";
import { presentRow } from "#/modules/branches/branch-review";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";

/**
 * The Parent's deployed changes the Branch doesn't have, then each Live Node its owner redeployed since the Branch last
 * deployed. Read-only; #1160 adds Update.
 */
export function UpdateSection({ review }: { review: BranchReviewView }) {
  const parent = review.parent.name;
  return (
    <ReviewSection title={`New in ${parent}`} count={review.updates}
      help={review.live.length ? "This branch still runs on the values it captured from what it uses live. Deploying it picks up the new ones." : undefined}>
      {review.updates ? (
        <ItemGroup className="gap-1">
          {review.update.map((row) => <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)} />)}
          {review.live.map((live) => (
            <ChangeRowItem key={`live:${live.lineageId}`}
              row={{ key: live.lineageId, lineageId: live.lineageId, node: review.nameOf(live.lineageId), label: "Used live", before: "", after: "" }}
              description={<ItemDescription>
                {review.environmentName(live.ownerEnvironmentId)} deployed it <RelativeTime date={live.deployedAt} />
              </ItemDescription>} />
          ))}
        </ItemGroup>
      ) : <p className="text-sm text-muted-foreground">Nothing new in {parent}.</p>}
    </ReviewSection>
  );
}
