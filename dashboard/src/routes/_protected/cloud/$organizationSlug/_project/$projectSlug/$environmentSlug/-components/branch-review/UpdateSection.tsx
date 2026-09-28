import { useParams } from "@tanstack/react-router";
import { Button } from "#/components/ui/button";
import { ItemDescription, ItemGroup } from "#/components/ui/item";
import { useBranchUnsettled } from "#/modules/branches/branch.collection";
import { useUpdateBranch } from "#/modules/branches/branch-commands";
import { RelativeTime } from "#/components/relative-time";
import { presentRow } from "#/modules/branches/branch-review";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * The Parent's deployed changes the Branch doesn't have, then each Live Node its owner redeployed since the Branch last
 * deployed. Update stages the Parent's rows here; Live Nodes need only the Branch's next deploy.
 */
export function UpdateSection({ review, environmentId }: { review: BranchReviewView; environmentId: string }) {
  const parent = review.parent.name;
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { update } = useUpdateBranch(params.organizationSlug);
  const unsettled = useBranchUnsettled(params.organizationSlug, environmentId);
  return (
    <ReviewSection title={`New in ${parent}`} count={review.updates}
      help={review.live.length ? "Deploy to pick up the live ones." : undefined}>
      {review.updates ? (
        <ItemGroup className="gap-1">
          {review.update.map((row) => (
            <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)} conflict={row.role === "move" && row.conflict ? review.environmentName(environmentId) : undefined} />
          ))}
          {review.live.map((live) => (
            <ChangeRowItem key={`live:${live.lineageId}`}
              row={{ key: live.lineageId, lineageId: live.lineageId, node: review.nameOf(live.lineageId), label: "Used live", before: "", after: "" }}
              description={<ItemDescription>
                {review.environmentName(live.ownerEnvironmentId)} deployed it <RelativeTime date={live.deployedAt} />
              </ItemDescription>} />
          ))}
        </ItemGroup>
      ) : <p className="text-sm text-muted-foreground">Nothing new in {parent}.</p>}
      {review.update.length ? (
        <div className="flex flex-col items-start gap-2">
          <Button onClick={() => update(environmentId)} disabled={unsettled !== null}>Update from {parent}</Button>
          {unsettled ? <p className="text-sm text-muted-foreground">{unsettled}</p> : null}
        </div>
      ) : null}
    </ReviewSection>
  );
}
