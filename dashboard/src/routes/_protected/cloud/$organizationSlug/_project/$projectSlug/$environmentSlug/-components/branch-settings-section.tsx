import { Link } from "@tanstack/react-router";
import { ChevronRightIcon, GitBranchIcon } from "lucide-react";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Switch } from "#/components/ui/switch";
import { useKeepBranch } from "#/modules/branches/branch.collection";

/** A Branch's own settings: where it came from and lands, and whether it's kept. */
export function BranchSettingsSection({ organizationSlug, projectSlug, branch, parent }: {
  organizationSlug: string;
  projectSlug: string;
  branch: { environmentId: string; kept: boolean };
  parent: { name: string; namespace: string };
}) {
  const keepBranch = useKeepBranch(organizationSlug);
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
          <ItemDescription>Its changes land in {parent.name}.</ItemDescription>
        </ItemContent>
        <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
      </Item>
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel htmlFor="keep-branch">Keep this branch</FieldLabel>
          <FieldDescription>
            {branch.kept
              ? `It stays after you merge it into ${parent.name}.`
              : `It closes after you merge it into ${parent.name}, or 7 days after its last deploy.`}
          </FieldDescription>
        </FieldContent>
        <Switch id="keep-branch" checked={branch.kept}
          onCheckedChange={(kept) => keepBranch(branch.environmentId, kept)} />
      </Field>
    </section>
  );
}
