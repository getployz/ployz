import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { ChevronRightIcon, GitBranchIcon } from "lucide-react";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Switch } from "#/components/ui/switch";
import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { getPrEnvironmentPlansCollection, type BranchRow } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useKeepBranch } from "#/modules/branches/branch.collection";
import { defaultPrEnvironmentPlan } from "#/modules/pr-environments/repositories";

/** A Branch's own settings: where it came from, whether it's kept, and how it closes. A PR Environment has no Keep. */
export function BranchSettingsSection({ organizationSlug, projectSlug, branch, parent, children }: {
  organizationSlug: string;
  projectSlug: string;
  branch: BranchRow;
  parent: { name: string; namespace: string };
  /** Its close row, when it closes from here rather than from Danger. */
  children?: ReactNode;
}) {
  const keepBranch = useKeepBranch(organizationSlug);
  const { data: plans } = useLiveSuspenseQuery(getPrEnvironmentPlansCollection(organizationSlug, useCollectionScope()));
  const { pullRequest } = branch;
  const { removeOnClose } = plans.find((plan) => plan.projectId === branch.projectId && plan.repositoryId === pullRequest?.repositoryId)
    ?? defaultPrEnvironmentPlan;
  return (
    <section aria-labelledby="branch-heading" className="flex flex-col gap-4">
      <h2 id="branch-heading" className="text-lg font-semibold">Branch</h2>
      <Item variant="outline" size="sm" render={
        <Link to="/cloud/$organizationSlug/$projectSlug/$environmentSlug"
          params={{ organizationSlug, projectSlug, environmentSlug: parent.namespace }} />
      }>
        <ItemMedia variant="icon"><GitBranchIcon /></ItemMedia>
        <ItemContent className="min-w-0">
          <ItemTitle>From {parent.name}</ItemTitle>
          {pullRequest && <ItemDescription>
            {`PR environment for #${pullRequest.number} · ${pullRequest.title} · ${removeOnClose ? "removed when it closes" : "stays 7 days after its last deploy"}`}
          </ItemDescription>}
        </ItemContent>
        <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
      </Item>
      {!pullRequest && <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel htmlFor="keep-branch">Keep this branch</FieldLabel>
          <FieldDescription>Otherwise it can be deleted after saving, and closes after 7 days without a deploy.</FieldDescription>
        </FieldContent>
        <Switch id="keep-branch" checked={branch.kept}
          onCheckedChange={(kept) => keepBranch(branch.environmentId, kept)} />
      </Field>}
      {children}
    </section>
  );
}
