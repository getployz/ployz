import type { BranchReviewView, PullRequest } from "#/modules/branches/use-branch-review";
import { ReviewSection } from "./BranchReviewPanel";
import { RowPicks, useRowPicks } from "./RowPicks";

/**
 * What a PR Environment's pull request would move into one Destination when it merges, change by change, each with a tick
 * and each variable a value choice. Nothing moves until someone approves; `rows.sent` and `landing.review` are what an
 * approval sends.
 */
export function GoesToSection({ review, pr, landing, name }: {
  review: BranchReviewView;
  pr: PullRequest;
  landing: BranchReviewView["goesTo"][number];
  /** The PR Environment's name. */
  name: string;
}) {
  const destination = landing.destination.name;
  const rows = useRowPicks(landing.rows, review.nameOf);
  return (
    <ReviewSection title={`Goes to ${destination} when #${pr.number} merges`} count={landing.rows.length}
      help={landing.rows.length ? `Nothing moves until someone approves. Unticked changes stay in ${name}.` : undefined}>
      {landing.rows.length
        ? <RowPicks picks={rows} names={{ from: name, parent: review.parent.name, destination }} verb={`Send to ${destination}`} />
        : <p className="text-sm text-muted-foreground">Nothing here that {destination} doesn't have.</p>}
    </ReviewSection>
  );
}
