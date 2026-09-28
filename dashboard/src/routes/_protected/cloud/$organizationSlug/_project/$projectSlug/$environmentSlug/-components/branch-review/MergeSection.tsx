import { useState } from "react";
import { useParams } from "@tanstack/react-router";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { useSaveBranch } from "#/modules/branches/branch-commands";
import { useBranchUnsettled } from "#/modules/branches/branch.collection";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { ReviewSection } from "./BranchReviewPanel";
import { RowPicks, useRowPicks } from "./RowPicks";

/**
 * What would merge into the Destination, change by change: the Branch's Working State against the Destination's, over the
 * base. Each change has a tick and each variable a value choice. Merge stages the ticked ones in the Destination, and is
 * open only while the Branch runs exactly its Working State.
 */
export function MergeSection({ review, branch }: { review: BranchReviewView; branch: { id: string; name: string } }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const destination = review.parent.name;
  const unsettled = useBranchUnsettled(params.organizationSlug, branch.id);
  const isDefault = useWorkspace(params.organizationSlug).projects.some((project) => project.defaultEnvironmentId === branch.id);
  const merge = useSaveBranch({ organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, branchName: branch.name, destination: review.parent });
  const rows = useRowPicks(review.save, review.nameOf);
  const [thenDelete, setThenClose] = useState(true);

  const closes = !review.kept && !isDefault && thenDelete;
  const blocked = unsettled
    ?? (rows.ticked.length === 0 ? "Tick a change to merge."
    : rows.missing ? `Enter a new value for ${rows.missing.presented.label}.`
    : null);

  function submit() {
    merge.mutate({
      branchEnvironmentId: branch.id, review: review.saveReview, thenDelete: closes, picks: rows.sent,
    });
  }

  return (
    <ReviewSection title={`Merge into ${destination}`} count={review.save.length}>
      {review.save.length ? (
        <>
          <RowPicks picks={rows} names={{ from: branch.name, parent: destination, destination }} verb="Merge" />
          {review.kept ? null : (
            <FieldLabel htmlFor="merge-then-close">
              <Field orientation="horizontal" data-disabled={isDefault || undefined}>
                <FieldContent>
                  <span>Then close {branch.name}</span>
                  <FieldDescription>
                    {isDefault ? "The Default Environment stays open." : "Deletes its services and data."}
                  </FieldDescription>
                </FieldContent>
                <Switch id="merge-then-close" checked={closes} disabled={isDefault} onCheckedChange={setThenClose} />
              </Field>
            </FieldLabel>
          )}
          <div className="flex flex-col gap-2">
            <Button className="self-start" disabled={blocked !== null || merge.isPending} onClick={submit}>
              {merge.isPending ? <Spinner data-icon="inline-start" /> : null}Merge into {destination}
            </Button>
            {blocked ? <FieldDescription>{blocked}</FieldDescription>
              : <FieldDescription>Stages the changes in {destination}.</FieldDescription>}
            {merge.isError ? <FieldError>{merge.error.message}</FieldError> : null}
          </div>
        </>
      ) : <p className="text-sm text-muted-foreground">No changes.</p>}
    </ReviewSection>
  );
}
