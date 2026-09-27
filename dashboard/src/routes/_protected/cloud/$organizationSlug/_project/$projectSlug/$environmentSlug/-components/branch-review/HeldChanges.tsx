import { Link, useParams } from "@tanstack/react-router";
import { buttonVariants } from "#/components/ui/button-variants";
import { ItemGroup } from "#/components/ui/item";
import { presentRow } from "#/modules/branches/branch-review";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useHeldChanges, useStagedInstead } from "#/modules/pr-environments/conditional-save.collection";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";

/** "Waiting for #142 · 3 changes · approved by maya": one approval held here, for the bottom bar and the review. */
export function waitingLine(save: ConditionalSaveRow) {
  const n = save.rows.length;
  return { title: `Waiting for #${save.prNumber}`, detail: `${n} ${n === 1 ? "change" : "changes"} · approved by ${save.approvedBy ?? "a former member"}` };
}

/**
 * Changes approved for pull requests and held on this Destination until each merges. Read-only and neutral: nothing is
 * staged here, and they're changed in the PR Environment's review. First, the rows a merge staged instead of saving.
 */
export function HeldChanges({ environmentId }: { environmentId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const held = useHeldChanges(params.organizationSlug, environmentId);
  const stagedInstead = useStagedInstead(params.organizationSlug, environmentId);
  const documents = useEnvironmentDocuments(params.organizationSlug);
  const lineageName = useLineageNames(params.organizationSlug);
  const rowsOf = (save: ConditionalSaveRow) => (
    <ItemGroup className="gap-1">
      {save.rows.map(({ row }) => <ChangeRowItem key={row.key} row={presentRow(row, (lineage) => lineageName(lineage, save.prEnvironmentId ?? undefined))} />)}
    </ItemGroup>
  );
  const changed = stagedInstead.map((save) => (
    <ReviewSection key={save.id} title={`Changed since #${save.prNumber} was approved`} count={save.rows.length}
      help={`#${save.prNumber} merged, but these changed here after its approval, so they're staged, not saved. Save or discard them.`}>
      {rowsOf(save)}
    </ReviewSection>
  ));
  return [...changed, ...held.map((save) => {
    const pr = documents.find((document) => document.id === save.prEnvironmentId);
    const line = waitingLine(save);
    return (
      <ReviewSection key={save.id} title={line.title} count={save.rows.length}
        help={`Approved by ${save.approvedBy ?? "a former member"}. They land when #${save.prNumber} merges, and change only in its review.`}>
        {rowsOf(save)}
        {pr ? (
          <Link to={ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO} params={{ ...params, environmentSlug: pr.namespace }}
            className={buttonVariants({ size: "sm", variant: "outline", className: "self-start" })}>
            Open #{save.prNumber}'s review
          </Link>
        ) : null}
      </ReviewSection>
    );
  })];
}
