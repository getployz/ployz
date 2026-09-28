import { useState } from "react";
import { useParams } from "@tanstack/react-router";
import { Button } from "#/components/ui/button";
import { ItemGroup } from "#/components/ui/item";
import { presentRow, type ChangeRow } from "#/modules/branches/branch-review";
import type { BranchReviewView, PullRequest } from "#/modules/branches/use-branch-review";
import { useConditionalSave } from "#/modules/pr-environments/conditional-save-commands";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";
import { PrSaveSheet, SaveSheet } from "./SaveSheet";

type Landing = BranchReviewView["goesTo"][number];

/**
 * What Save would put where the Branch saves: its Parent, or each Destination of a PR Environment while its pull request
 * is open. Save opens the sheet; what a PR Environment saved waits for the merge, and Undo takes it back.
 */
export function SaveSection({ review, environmentId }: { review: BranchReviewView; environmentId: string }) {
  // The Save sheet open: for the Parent, or a PR Environment's Destination (its id).
  const [saving, setSaving] = useState<string | null>(null);
  const pr = review.pullRequest;
  if (pr?.closed) return null;
  const landings = pr ? review.goesTo.filter((landing) => landing.saved || landing.rows.length) : [];
  if (!pr || !landings.length) {
    const rows = pr ? [] : review.save;
    return (
      <ReviewSection title={`For ${review.parent.name}`} count={rows.length}>
        {rows.length ? <Rows rows={rows} review={review} into={review.parent.name} /> : <p className="text-sm text-muted-foreground">Nothing to save.</p>}
        {rows.length ? <Button className="self-start" onClick={() => setSaving(review.parent.id)}>Save to {review.parent.name}</Button> : null}
        {saving ? <SaveSheet review={review} branchId={environmentId} onClose={() => setSaving(null)} /> : null}
      </ReviewSection>
    );
  }
  return landings.map((landing) => landing.saved
    ? <SavedLanding key={landing.destination.id} review={review} landing={landing} saved={landing.saved} pullRequest={pr} environmentId={environmentId} />
    : (
      <ReviewSection key={landing.destination.id} title={`For ${landing.destination.name}`} count={landing.rows.length}
        help={`Goes live when PR #${pr.number} merges.`}>
        <Rows rows={landing.rows} review={review} into={landing.destination.name} />
        <Button className="self-start" onClick={() => setSaving(landing.destination.id)}>Save to {landing.destination.name}</Button>
        {saving === landing.destination.id
          ? <PrSaveSheet review={review} branchId={environmentId} landing={landing} pullRequest={pr} onClose={() => setSaving(null)} /> : null}
      </ReviewSection>
    ));
}

/** A PR Environment's saved changes for one Destination: they go live when its pull request merges, until Undo. */
function SavedLanding({ review, landing, saved, pullRequest, environmentId }: {
  review: BranchReviewView; landing: Landing; saved: ConditionalSaveRow; pullRequest: PullRequest; environmentId: string;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { withdraw } = useConditionalSave({
    organizationSlug: params.organizationSlug, prEnvironmentId: environmentId, destinationEnvironmentId: landing.destination.id,
  });
  return (
    <ReviewSection title={`Saved for ${landing.destination.name}`} count={saved.rows.length} help={`Goes live when PR #${pullRequest.number} merges.`}>
      <Rows rows={saved.rows.map(({ row }) => row)} review={review} into={landing.destination.name} />
      <Button variant="outline" className="self-start" disabled={withdraw.isPending} onClick={() => withdraw.mutate()}>Undo</Button>
    </ReviewSection>
  );
}

/** Each change as the receiver would get it, marked where the receiver changed it too since branching. */
function Rows({ rows, review, into }: { rows: ChangeRow[]; review: BranchReviewView; into: string }) {
  return (
    <ItemGroup className="gap-1">
      {rows.map((row) => (
        <ChangeRowItem key={row.key} row={presentRow(row, review.nameOf)} conflict={row.role === "move" && row.conflict ? into : undefined} />
      ))}
    </ItemGroup>
  );
}
